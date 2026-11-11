#!/usr/bin/env python3
"""Post-commit source negative controls for #164.

This runner mutates the tracked apparatus source in place and requires the
behavior check to turn red for the specific mutation, not for an unrelated
error.  It must run only after the #164 changes are committed:
``mutating_sources`` proves the named file is byte-identical to its baseline
before writing anything, and refuses otherwise.  The check_linux.py summary is a mutation target; every selected source must
match committed HEAD before the existing snapshot guard admits mutation.

Run only after the coordinator commits the patch; never substitute a worktree
snapshot for HEAD to bypass the pristine-source guard.

Run on the coordinator's host (no cargo is launched):

  python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/negative_controls.py \
    --output _tmp/20261026-issue-164/negative/source

``--only NAME`` (repeatable) restricts the source mutations to the named rows;
the restored positive still runs.  ``MUTATIONS`` is the literal
``(name, source, old, new)`` table read by #820's anchor selfcheck, and
``JUDGES`` holds the mode/judge/launcher metadata keyed by mutation name.

Exit codes: 0 every mutant killed and the restored positive is PASS; 1 a
mutant survived, the restored source failed, or the restored positive only
passed with platform skips (PASS_WITH_SKIPS); 2 an anchor is stale (refuse to
mutate) or a case is INVALID/BLOCKED (environment/startup failure, never
counted as KILLED).  The top-level status and exit code come from the one
shared ``common.aggregate_status()``; the restored positive is a child too.
"""
import argparse
import atexit
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time

# Same private bytecode root as check_cleanup.py: the apparatus modules this
# process loads each install their own ``sys.pycache_prefix`` directory, and
# only a root that owns them all can clean them all (MiniMax sibling r7 P2-2).
_PYCACHE_ROOT = tempfile.mkdtemp(prefix='pdrs-session-cleanup-neg-pycroot-')
tempfile.tempdir = _PYCACHE_ROOT
sys.pycache_prefix = _PYCACHE_ROOT
atexit.register(shutil.rmtree, _PYCACHE_ROOT, ignore_errors=True)
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (KILL_TIMEOUT, defer_signal_exit, mutating_sources,
                             restore_critical, run_owned, take_deferred_signal,
                             wait_for_slot)
from negative_process import committed_status_contract

ROOT = Path(__file__).resolve().parents[5]
SOURCE = ROOT / 'scripts/gateway-parity/session_domain.py'
# #892 fix-r5: the one shared exit rebind in ``session_domain.py:main``.
# Every mutant below adds its own bypass path *in front of* this line so the
# healthy fall-through keeps its shared rebind and only the new path is under
# test; replacing the shared line outright would unmask the ``code = 0``
# initializer and pin the wrong rule.
A_EXIT = "                    _, code = common.aggregate_status(report['status'])\n"
B_STATUS = ("                report['status'] = 'BLOCKED' if "
            "isinstance(error, BLOCKED_APPARATUS_ERRORS) else 'FAIL'\n")
CONTROLS_SOURCE = ROOT / 'scripts/gateway-parity/controls/session_domain.py'
# #892 fix-r6: the one shared exit in ``controls/session_domain.py:main``.
CT_EXIT = "        return common.aggregate_status(report['status'])[1]\n"
CT_MAIN = "def main():\n"
COMMON_SOURCE = ROOT / 'scripts/gateway-parity/common.py'
# #892 fix-r15: a second audited file, so the cross-file registry key and the
# exit-call enumeration are exercised where the real ``rel`` is not the fixture
# itself.
GUARD_SOURCE = ROOT / 'scripts/gateway-parity/_guard.py'
LINUX_SOURCE = ROOT / 'scripts/gateway-parity/linux_group_cleanup.py'
LINUX_CHECK = Path(__file__).resolve().with_name('check_linux.py')
CHECK = Path(__file__).resolve().with_name('check_cleanup.py')
NEGATIVE = Path(__file__).resolve()
# #892 fix-r25: the ``rel`` the audit reports for the two fixture files, so a
# judge can bind the failure to the file the mutant touched instead of any
# ``main`` in the batch.
CHECK_REL = ('src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/'
             'check_cleanup.py')
NEG_REL = ('src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/'
           'negative_controls.py')
# #892 fix-r26: the file that *defines* the cross-file proven source, and the
# ``rel`` the audit reports for it -- a judge binds the failure to it instead
# of trusting any single definition in the batch.
NEGATIVE_PROCESS = (ROOT / 'src/pilotdeck-gateway/tests/fixtures/'
                    'negative_process.py')
NEGATIVE_PROCESS_REL = ('src/pilotdeck-gateway/tests/fixtures/'
                        'negative_process.py')
GUARD_REL = 'scripts/gateway-parity/_guard.py'
# #892 fix-r27: the controls module's ``rel``, so a judge binds the shared-name
# withdrawal to the mutant that caused it.
CONTROLS_REL = 'scripts/gateway-parity/controls/session_domain.py'

_MISSING = object()


def dig(row, path):
    current = row
    for part in path.split('.'):
        if not isinstance(current, dict):
            return _MISSING
        current = current.get(part, _MISSING)
    return current


def match(rows, mode, expected):
    """One row for *mode* with exactly the mutant's expected (wrong) fields."""
    matched = [row for row in rows if row.get('mode') == mode]
    if len(matched) != 1:
        return False, f'mode {mode}: expected 1 result row, saw {len(matched)}'
    row = matched[0]
    observed = {}
    for path, want in expected.items():
        got = dig(row, path)
        observed[path] = None if got is _MISSING else got
        if got != want:
            return False, f'{path}={observed[path]!r}, want {want!r}'
    if row.get('passed') is not False:
        return False, f"passed={row.get('passed')!r}, want False"
    return True, (f"{mode}: "
                  + ', '.join(f'{path}={observed[path]!r}' for path in expected)
                  + ', passed=False')


def status_mapping(rows):
    """The relaxed helper must turn both non-VERIFIED rows into PASS."""
    matched = [row for row in rows if row.get('mode') == 'status']
    by_state = {row.get('processTreeCleanup'): row for row in matched}
    for state in ('GROUP_NOT_EMPTY', 'BLOCKED'):
        row = by_state.get(state)
        if row is None:
            return False, f'status: missing {state} row'
        if row.get('observed') != 'PASS':
            return False, f'{state}: observed={row.get("observed")!r}, want PASS'
        if row.get('passed') is not False:
            return False, f'{state}: passed={row.get("passed")!r}, want False'
    return True, 'status: GROUP_NOT_EMPTY/BLOCKED -> observed=PASS, passed=False'


def blocked_branch_removed(rows):
    """Removing the generic BLOCKED branch raises instead of returning BLOCKED."""
    matched = [row for row in rows if row.get('mode') == 'blocked']
    if len(matched) != 1:
        return False, f'blocked: expected 1 result row, saw {len(matched)}'
    row = matched[0]
    error = row.get('error')
    if not (isinstance(error, str) and error.startswith('RuntimeError:')):
        return False, f'error={error!r}, want a RuntimeError from the removed BLOCKED branch'
    reported_cases = row.get('cases') or []
    if [case.get('case') for case in reported_cases] != ['permission-denied']:
        return False, f"cases={[case.get('case') for case in reported_cases]!r}, want ['permission-denied']"
    if row.get('passed') is not False:
        return False, f"passed={row.get('passed')!r}, want False"
    return True, f'blocked: error={error!r}, cases=[permission-denied], passed=False'


def wiring_relaxed(rows):
    """`overall = 'PASS'` must show up in the real report tail."""
    return match(rows, 'wiring', {
        'status': 'PASS',
        'executionStatus': 'PASS',
        'freezeStatus': 'DRAFT_AWAITING_A5_4',
        'returncode': 0,
    })


def _audit_row(rows):
    matched = [row for row in rows if row.get('mode') == 'aggregation-audit']
    if len(matched) != 1:
        return None, f'aggregation-audit: expected 1 result row, saw {len(matched)}'
    return matched[0], None


def summary_dict_bypass(rows):
    """#892: a directly returned summary dict must fail the audit as an
    unregistered point, not pass silently.

    r1: the mutant relays ``report['status']`` into the real summary, which is a
    legal relay now that status slots are checked — so this judge binds only the
    enumeration layer; the slot layer has its own ``summary-name-bypass``.
    """
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    failures = row.get('failures') or []
    if not any(failure.startswith(
            '#164-fix-r7 aggregation-audit: unregistered aggregation point '
            'scripts/gateway-parity/controls/session_domain.py:inline_batch_summary')
            for failure in failures):
        return False, f'failures={failures!r}, want the unregistered inline_batch_summary point'
    return True, ('aggregation-audit: inline_batch_summary unregistered, '
                  'passed=False')


def branch_exit_inline(rows):
    """#892: a branch computing its own exit code must fail the audit even
    while another branch still calls the shared function."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    failures = row.get('failures') or []
    if not any(failure.startswith(
            '#892 aggregation-audit: '
            'scripts/gateway-parity/controls/session_domain.py:main ')
            # r2 judges each conditional arm on its own, so a non-pair inline
            # code reports ``inline exit code ...`` instead of the pair wording
            # ``inline status/exit ...``; both are this rule.
            and ('inline status/exit' in failure or 'inline exit code' in failure)
            for failure in failures):
        return False, f'failures={failures!r}, want the #892 main inline status/exit failure'
    slots = (row.get('points', {}).get('controls-main', {}) or {}).get('unsharedSlots')
    if not slots:
        return False, 'points.controls-main.unsharedSlots is empty, want the inline exit branch'
    return True, 'aggregation-audit: main inline exit branch flagged, passed=False'


def summary_name_bypass(rows):
    """#892 r1: a summary dict built into a local name and returned by name must
    still have its status slot checked (the dominant real-code shape)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    failures = row.get('failures') or []
    if not any(failure.startswith(
            '#892 aggregation-audit: '
            'scripts/gateway-parity/controls/session_domain.py:run_all ')
            and "found status slot 'PASS'" in failure
            for failure in failures):
        return False, f'failures={failures!r}, want the #892 run_all name-returned slot failure'
    slots = (row.get('points', {}).get('controls-run-all', {}) or {}).get('unsharedSlots')
    if not slots:
        return False, 'points.controls-run-all.unsharedSlots is empty, want the name-returned slot'
    if any(failure.startswith('#164-fix-r7 aggregation-audit: unregistered')
           for failure in failures):
        return False, f'failures={failures!r}, want no unregistered point in this mutant'
    return True, ('aggregation-audit: the status slot of a name-returned summary dict '
                  'flagged, passed=False')


def bare_exit_zero(rows):
    """#892 r1: a bare ``return 0`` exit path must be flagged even when another
    branch still computes the code through the shared function."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    failures = row.get('failures') or []
    if not any(failure.startswith(
            '#892 aggregation-audit: '
            'scripts/gateway-parity/controls/session_domain.py:main ')
            and ('inline exit code 0' in failure
                 or 'inline optimistic exit 0' in failure)
            for failure in failures):
        return False, f'failures={failures!r}, want the #892 main bare exit-code failure'
    slots = (row.get('points', {}).get('controls-main', {}) or {}).get('unsharedSlots')
    if not slots:
        return False, 'points.controls-main.unsharedSlots is empty, want the bare exit code'
    return True, 'aggregation-audit: bare optimistic exit code flagged, passed=False'


def _main_failure(row, needle, point='controls-main', function='main'):
    """The #892 *function* failure whose text contains *needle*, if any."""
    failures = row.get('failures') or []
    if not any(failure.startswith(
            '#892 aggregation-audit: '
            f'scripts/gateway-parity/controls/session_domain.py:{function} ')
            and needle in failure for failure in failures):
        return False, f'failures={failures!r}, want the #892 {function} failure with {needle!r}'
    slots = (row.get('points', {}).get(point, {}) or {}).get('unsharedSlots')
    if not slots:
        return False, f'points.{point}.unsharedSlots is empty, want it flagged'
    return True, None


def _point_failure(row, file, function, needle, point):
    """The #892 *function* failure in *file* whose text contains *needle*."""
    failures = row.get('failures') or []
    if not any(failure.startswith(f'#892 aggregation-audit: {file}:{function} ')
               and needle in failure for failure in failures):
        return False, (f'failures={failures!r}, want the #892 {file}:{function} '
                       f'failure with {needle!r}')
    slots = (row.get('points', {}).get(point, {}) or {}).get('unsharedSlots')
    if not slots:
        return False, f'points.{point}.unsharedSlots is empty, want it flagged'
    return True, None


def status_slot_relayed(rows):
    """#892 r1: a status slot filled from a locally computed variable must be
    flagged — the shared call elsewhere in the function cannot excuse it."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    # The slot here is the local variable ``status`` (an ``ast.Name``), which
    # unparses without quotes — unlike the ``'PASS'`` constant that
    # ``summary-name-bypass`` pins.
    ok, detail = _main_failure(row, 'status slot status at line',
                               'controls-run-all', 'run_all')
    if not ok:
        return ok, detail
    return True, 'aggregation-audit: locally computed status slot flagged, passed=False'


def system_exit_zero_bypass(rows):
    """#892 r1: ``raise SystemExit(0)`` never returns through the shared map."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _main_failure(row, 'inline optimistic exit (SystemExit)')
    if not ok:
        return ok, detail
    return True, 'aggregation-audit: optimistic raise SystemExit flagged, passed=False'


def bare_return_exit(rows):
    """#892 r1: a bare ``return`` hands back None, which exits 0."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _main_failure(row, 'inline optimistic exit (bare return)')
    if not ok:
        return ok, detail
    return True, 'aggregation-audit: bare return exit flagged, passed=False'


def conditional_arm_exit(rows):
    """#892 r1: each arm of a conditional exit is judged on its own."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _main_failure(row, 'inline exit code int(')
    if not ok:
        return ok, detail
    return True, 'aggregation-audit: optimistic conditional arm flagged, passed=False'


def calibrate_bare_zero(rows):
    """#892 r1: the same optimistic exit on the calibrate entry point."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    failures = row.get('failures') or []
    if not any(failure.startswith(
            '#892 aggregation-audit: scripts/gateway-parity/session_domain.py:main ')
            and ('inline exit code 0' in failure
                 or 'inline optimistic exit 0' in failure)
            for failure in failures):
        return False, f'failures={failures!r}, want the #892 calibrate-main bare exit failure'
    slots = (row.get('points', {}).get('calibrate-main', {}) or {}).get('unsharedSlots')
    if not slots:
        return False, 'points.calibrate-main.unsharedSlots is empty, want the bare exit code'
    return True, 'aggregation-audit: calibrate bare optimistic exit flagged, passed=False'


def calibrate_status_write_pass(rows):
    """#892 r2: an incremental ``report['status'] = 'PASS'`` write inside a
    registered point never appears in a Return, so the per-return-path audit
    must scan the assignment itself (DeepSeek sibling P1-2)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    failures = row.get('failures') or []
    if not any(failure.startswith(
            '#892 aggregation-audit: scripts/gateway-parity/session_domain.py:main ')
            and "inline status/exit assignment 'PASS'" in failure
            for failure in failures):
        return False, f'failures={failures!r}, want the #892 calibrate status-write failure'
    slots = (row.get('points', {}).get('calibrate-main', {}) or {}).get('unsharedSlots')
    if not slots:
        return False, 'points.calibrate-main.unsharedSlots is empty, want the status write'
    return True, ('aggregation-audit: inline report-status write flagged, '
                  'passed=False')


def controls_return_none(rows):
    """#892 r3: ``return None`` from a registered point is exit 0 to the shell
    (DeepSeek boundary P0-1); only ``_guard._failure`` keeps the sentinel."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _main_failure(row, 'inline optimistic exit None')
    if not ok:
        return ok, detail
    return True, 'aggregation-audit: explicit return None flagged, passed=False'


def controls_system_exit_none(rows):
    """#892 r3: ``raise SystemExit(None)`` is exit 0 too (DeepSeek P0-2)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _main_failure(row, 'inline optimistic exit (SystemExit)')
    if not ok:
        return ok, detail
    return True, ('aggregation-audit: raise SystemExit(None) flagged, '
                  'passed=False')


def calibrate_exit_code_relayed(rows):
    """#892 r3: an exit code computed into a local variable and returned by
    name is judged by how the variable is bound, not by its name."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _point_failure(
        row, 'scripts/gateway-parity/session_domain.py', 'main',
        'inline exit code (conditional relay)', 'calibrate-main')
    if not ok:
        return ok, detail
    return True, ('aggregation-audit: relayed exit code flagged, '
                  'passed=False')


def controls_status_slot_subscript(rows):
    """#892 r3: a status slot filled from a local dict subscript hides the
    clean verdict from the literal rule (DeepSeek boundary P1-1)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _main_failure(
        row, "status slot local['status']", 'controls-run-all', 'run_all')
    if not ok:
        return ok, detail
    return True, ('aggregation-audit: subscript status slot flagged, '
                  'passed=False')


def controls_aggregate_lookalike(rows):
    """#892 r3: only ``common.aggregate_status`` counts; a local function with
    the same name must not satisfy the shared-call rule (MiniMax P1)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    failures = row.get('failures') or []
    if not any(failure.startswith(
            '#892 aggregation-audit: '
            'scripts/gateway-parity/controls/session_domain.py defines its own '
            'aggregate_status')
            for failure in failures):
        return False, f'failures={failures!r}, want the local aggregate_status failure'
    return True, ('aggregation-audit: local aggregate_status lookalike flagged, '
                  'passed=False')


def calibrate_status_update(rows):
    """#892 r3: ``report.update({'status': 'PASS'})`` writes the slot without a
    ``Subscript`` target (MiniMax P1)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _point_failure(
        row, 'scripts/gateway-parity/session_domain.py', 'main',
        "inline status/exit assignment report.update({'status': 'PASS'})",
        'calibrate-main')
    if not ok:
        return ok, detail
    return True, ('aggregation-audit: update() status write flagged, '
                  'passed=False')


def calibrate_shared_excuse(rows):
    """#892 r4: a zero branch is still inline when the other branch is shared
    (DeepSeek boundary P0-1 / concurrency P1-1)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _point_failure(
        row, 'scripts/gateway-parity/session_domain.py', 'main',
        'inline exit code (literal relay) code', 'calibrate-main')
    if not ok:
        return ok, detail
    return True, ('aggregation-audit: zero branch beside a shared branch flagged, '
                  'passed=False')


def calibrate_status_setdefault(rows):
    """#892 r4: ``report.setdefault('status', 'PASS')`` writes the slot without
    any subscript target (DeepSeek boundary P0-2)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _point_failure(
        row, 'scripts/gateway-parity/session_domain.py', 'main',
        "report.setdefault('status', 'PASS')", 'calibrate-main')
    if not ok:
        return ok, detail
    return True, ('aggregation-audit: setdefault() status write flagged, '
                  'passed=False')


def _controls_main_needle(rows, needle, detail):
    ok, reason = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, reason
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, reason = _main_failure(row, needle)
    if not ok:
        return ok, reason
    return True, detail


def calibrate_attr_head_callee(rows):
    """#892 r13: ``report['status'] = helpers._clean()`` was exempt because the
    fail-closed call rule only knew bare-name callees (GLM boundary r12
    P0-1)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _point_failure(
        row, 'scripts/gateway-parity/session_domain.py', 'main',
        'inline status/exit assignment helpers._clean()', 'calibrate-main')
    if not ok:
        return ok, detail
    return True, ('aggregation-audit: attribute-headed clean call flagged, '
                  'passed=False')


def calibrate_mixed_branch_helper(rows):
    """#892 r13: a helper that raises on one branch and falls off the end on the
    other still exits 0 (GLM boundary r12 P0-2)."""
    return _controls_main_needle(
        rows, 'inline exit code _code(',
        'aggregation-audit: mixed-branch helper flagged, passed=False')


def calibrate_cross_module_name(rows):
    """#892 r13: a registered point's name in *another* module is not a proven
    slot source (GLM boundary r12 P1-1)."""
    return _controls_main_needle(
        rows, 'inline status/exit assignment record(report)',
        'aggregation-audit: cross-module lookalike callee flagged, passed=False')


def calibrate_exempt_callee_write(rows):
    """#892 r13: the exemption list may not double as the proven-callee list
    (MiniMax sibling r12 P1-1)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _point_failure(
        row, 'scripts/gateway-parity/session_domain.py', 'main',
        'inline status/exit assignment _retirement_census_verdict(report)', 'calibrate-main')
    if not ok:
        return ok, detail
    return True, ('aggregation-audit: exempt-callee clean write flagged, '
                  'passed=False')


def calibrate_quit_exit(rows):
    """#892 r14: ``quit(0)`` is the same site builtin exit as ``exit(0)`` and
    used to skip every rule in statement position (MiniMax boundary r13
    P0-1)."""
    return _controls_main_needle(
        rows, 'optimistic exit (quit(0))',
        'aggregation-audit: site quit() exit flagged, passed=False')


def calibrate_partial_raise_point(rows):
    """#892 r14: a registered point that raises on one branch and falls off
    the end on the other still exits 0 somewhere (MiniMax sibling r13 P1-1)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _point_failure(
        row, 'scripts/gateway-parity/controls/session_domain.py', 'run_all',
        'no return or exit path', 'controls-run-all')
    if not ok:
        return ok, detail
    return True, ('aggregation-audit: partially raising point flagged, '
                  'passed=False')


def calibrate_param_augassign_opaque(rows):
    """#892 r13: ``report |= payload`` on a parameter receiver is the opaque
    form of the r12 literal fix (MiniMax sibling r12 P1-3)."""
    return _unregistered_point(
        rows, 'publish', 'write-status',
        'aggregation-audit: parameter-receiver opaque |= enumeration '
        'flagged, passed=False')


def calibrate_helper_local_relay(rows):
    """#892 r13: a helper that stores 0 in a local and returns the name leaks
    exactly like a literal zero one call away (MiniMax boundary r12 P0-1)."""
    return _controls_main_needle(
        rows, 'inline exit code _relay(',
        'aggregation-audit: helper local-relay exit flagged, passed=False')


def calibrate_helper_bare_sysexit(rows):
    """#892 r13: a bare ``raise SystemExit`` inside a helper used to end the
    audit in an AttributeError instead of a verdict (MiniMax boundary r12
    P1-1)."""
    return _controls_main_needle(
        rows, 'inline exit code _bail(',
        'aggregation-audit: helper bare SystemExit flagged, passed=False')


def calibrate_boolean_exit(rows):
    """#892 r6: ``return not ok`` computes the exit code inline (GLM r5 P2-3 /
    MiniMax r5 P1-1)."""
    return _controls_main_needle(
        rows, 'inline exit code not ok',
        'aggregation-audit: boolean exit expression flagged, passed=False')


def calibrate_sys_exit_relay(rows):
    """#892 r6: ``sys.exit(holder['code'])`` reuses the exit-code provenance
    rules (GLM sibling r5 P1-2)."""
    return _controls_main_needle(
        rows, "inline exit code holder['code']",
        'aggregation-audit: sys.exit subscript relay flagged, passed=False')


def calibrate_system_exit_bare(rows):
    """#892 r6: ``raise SystemExit`` without arguments exits 0 too (GLM
    sibling r5 P1-3 / boundary r5 P1-1)."""
    return _controls_main_needle(
        rows, 'inline optimistic exit (SystemExit)',
        'aggregation-audit: bare raise SystemExit flagged, passed=False')


def calibrate_delegated_chain(rows):
    """#892 r6: an exit code delegated twice is followed to the leaf (MiniMax
    boundary r5 P1-2)."""
    return _controls_main_needle(
        rows, 'inline exit code _exit_a(report)',
        'aggregation-audit: two-hop delegated exit flagged, passed=False')


def calibrate_container_slot(rows):
    """#892 r6: a status slot filled from an unproven subscript is inline (GLM
    boundary r5 P1-2)."""
    return _controls_main_needle(
        rows, "inline status/exit assignment upstream['status']",
        'aggregation-audit: unproven subscript slot flagged, passed=False')


def calibrate_alias_cycle(rows):
    """#892 r6: a status slot fed by an alias cycle proves nothing (MiniMax
    sibling r5 P2-2: the rule needs an end-to-end mutant, not only a probe)."""
    # The cycle is caught on the summary side: ``return {'status': status}, code``
    # reads a name whose only binding is the swap itself, so the slot carries no
    # provenance (fail-closed, same rule as ``_name_relays``).
    return _controls_main_needle(
        rows, 'found status slot status',
        'aggregation-audit: alias-cycle status slot flagged, passed=False')


def calibrate_verdict_dict(rows):
    """#892 r6: a summary dict keyed ``verdict`` is an aggregation point too
    (GLM sibling r5 P1-1 / MiniMax sibling r5 P1)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    failures = row.get('failures') or []
    if not any('unregistered aggregation point' in failure
               and 'controls/session_domain.py:publish' in failure
               for failure in failures):
        return False, (f'failures={failures!r}, want the verdict-keyed summary '
                       'enumerated as an unregistered point')
    return True, ('aggregation-audit: verdict-keyed summary enumerated, '
                  'passed=False')


def calibrate_conditional_summary(rows):
    """#892 r7: a summary dict inside a conditional arm is still a summary
    (GLM r6 boundary P0-1)."""
    return _controls_main_needle(
        rows, "found status slot 'PASS'",
        'aggregation-audit: conditional summary dict flagged, passed=False')


def calibrate_update_expansion(rows):
    """#892 r7: ``update(**name)`` writes slots the audit cannot see, so it
    fails closed (GLM r6 boundary P1-2)."""
    return _controls_main_needle(
        rows, 'report.update(**extra)',
        'aggregation-audit: unprovable keyword expansion flagged, passed=False')


def calibrate_exit_conditional(rows):
    """#892 r7: ``sys.exit(0 if cond else 1)`` has a zero arm
    (GLM r6 sibling P1-1)."""
    return _controls_main_needle(
        rows, 'inline exit code 0 if',
        'aggregation-audit: conditional exit argument flagged, passed=False')


def calibrate_return_exit_call(rows):
    """#892 r7: ``return sys.exit(0)`` leaves the process with 0
    (GLM r6 sibling P1-2)."""
    return _controls_main_needle(
        rows, 'inline optimistic exit (sys.exit(0))',
        'aggregation-audit: exit call in return position flagged, passed=False')


def calibrate_ann_rebuilt(rows):
    """#892 r8: an annotated ``report: dict = {**report, ...}`` rebuild is the
    same write (MiniMax sibling r6 P2-1)."""
    return _controls_main_needle(
        rows, 'inline status/exit assignment {**report',
        'aggregation-audit: annotated rebuild flagged, passed=False')


def calibrate_bare_verdict(rows):
    """#892 r8: ``verdict = 'PASS'`` claims the batch verdict without a dict
    key (MiniMax sibling r6 P2-3)."""
    return _controls_main_needle(
        rows, "inline status/exit assignment 'PASS'",
        'aggregation-audit: bare verdict write flagged, passed=False')


def calibrate_bool_exit(rows):
    """#892 r8: ``return bool(x)`` exits 0 on a falsy value
    (MiniMax sibling r6 P2-2)."""
    return _controls_main_needle(
        rows, 'inline exit code bool(',
        'aggregation-audit: bool exit code flagged, passed=False')


def calibrate_bool_relay(rows):
    """#892 r9: ``code = bool(x); return code`` computes the code inline exactly
    like the ``int(...)`` relay (MiniMax boundary r7 P0-1 / sibling r7 P1-1)."""
    return _controls_main_needle(
        rows, '(bool() relay) code',
        'aggregation-audit: bool relay exit flagged, passed=False')


def calibrate_unbound_exit_name(rows):
    """#892 r9: an exit-code name with no local binding fails closed, the same
    verdict the status side gives it (GLM boundary r7 P1-1)."""
    return _controls_main_needle(
        rows, '(unbound relay) EXIT_CODE',
        'aggregation-audit: unbound exit-code name flagged, passed=False')


def calibrate_update_positional(rows):
    """#892 r9: ``report.update(x)`` writes the same slots as
    ``update(**x)``; only the keyword spelling was judged (GLM boundary r7 P1-2)."""
    return _controls_main_needle(
        rows, 'inline status/exit assignment report.update(extra)',
        'aggregation-audit: positional update flagged, passed=False')


def calibrate_augassign_update(rows):
    """#892 r9: ``report |= x`` is the in-place spelling of the same write
    (GLM boundary r7 P1-2)."""
    return _controls_main_needle(
        rows, 'inline status/exit update report |= extra',
        'aggregation-audit: augmented update flagged, passed=False')


def calibrate_tuple_verdict(rows):
    """#892 r9: a tuple target pairs element-wise, so ``verdict, code =
    'PASS', 0`` claims the verdict (GLM boundary r7 P2-3)."""
    return _controls_main_needle(
        rows, "inline status/exit assignment ('PASS', 0)",
        'aggregation-audit: tuple-target verdict write flagged, passed=False')


def calibrate_cleanup_word(rows):
    """#892 r9: the cleanup-verification word is on the clean side of the word
    family, so writing it into a slot is a bypass like ``'PASS'``
    (GLM boundary r7 P2-4)."""
    return _controls_main_needle(
        rows, "inline status/exit assignment 'PASS_WITH_CLEANUP_UNVERIFIED'",
        'aggregation-audit: cleanup-unverified word flagged, passed=False')


def calibrate_unbound_exit_relay(rows):
    """#892 r10: ``code = EXIT_OK; return code`` is the same unbound origin as
    the direct ``return EXIT_OK`` spelling (MiniMax boundary r9 P0-1)."""
    return _controls_main_needle(
        rows, '(unbound relay) code',
        'aggregation-audit: unbound exit-code relay flagged, passed=False')


def calibrate_no_exit_path(rows):
    """#892 r10: a registered point with no return and no exit leaves the entry
    point with None -- exit 0 -- and used to pass vacuously (MiniMax boundary
    r9 P1-2).

    The mutation strips ``run_all`` (``controls-run-all``), the registered
    point whose whole body is the batch; ``main`` keeps a second, non-anchor
    return so the same edit there would leave the rule silent.
    """
    ok, reason = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, reason
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, reason = _point_failure(
        row, 'scripts/gateway-parity/controls/session_domain.py', 'run_all',
        'no return or exit path', 'controls-run-all')
    if not ok:
        return ok, reason
    return True, 'aggregation-audit: no return or exit path flagged, passed=False'


def calibrate_helper_bare_return(rows):
    """#892 r11: a helper that falls off its end hands ``None`` back, so
    ``return check_case(...)`` exits 0 one call away (GLM boundary r10 P0-1).

    The helper is a real module-level function (``check_case`` ends in bare
    ``return`` statements), not a nested one: an unknown callee already fails
    closed in the delegation arm, so a nested definition would stay killed even
    if the bare-return branch regressed.
    """
    ok, reason = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, reason
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, reason = _point_failure(
        row, 'scripts/gateway-parity/controls/session_domain.py', 'main',
        'inline exit code check_case(args.case, args.output, args.source)',
        'controls-main')
    if not ok:
        return ok, reason
    return True, 'aggregation-audit: bare-return helper flagged, passed=False'


def calibrate_helper_exit_call(rows):
    """#892 r11: an exit spelling inside the helper body is the same exit 0 as
    the point's own ``raise SystemExit`` (GLM boundary r10 P1-2)."""
    ok, reason = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, reason
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, reason = _point_failure(
        row, 'scripts/gateway-parity/controls/session_domain.py', 'main',
        'inline exit code _bail()', 'controls-main')
    if not ok:
        return ok, reason
    return True, 'aggregation-audit: exit call inside a helper flagged, passed=False'


def calibrate_augassign_unregistered(rows):
    """#892 r11: ``report |= {'status': 'PASS'}`` is invisible to the ``Assign``
    family (``ast.AugAssign`` is a sibling), so an unregistered function using
    it was never enumerated (MiniMax boundary r10 P0-1)."""
    return _unregistered_point(
        rows, 'publish', 'write-status',
        'aggregation-audit: augmented |= enumeration flagged, passed=False')


def calibrate_opaque_clean_write(rows):
    """#892 r12: ``report['status'] = _clean()`` writes a clean verdict through
    a helper the audit never sees; every call used to count as a proven slot
    source, so the write stayed green (MiniMax boundary r11 P0-1)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _point_failure(
        row, 'scripts/gateway-parity/session_domain.py', 'main',
        'inline status/exit assignment _clean()', 'calibrate-main')
    if not ok:
        return ok, detail
    return True, 'aggregation-audit: opaque clean-status write flagged, passed=False'


def calibrate_opaque_update_payload(rows):
    """#892 r12: the same hole through ``report.update(_payload(report))``: an
    opaque call used to be a proven slot source as well (MiniMax boundary r11
    P0-1)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _point_failure(
        row, 'scripts/gateway-parity/session_domain.py', 'main',
        'report.update(_payload(report))', 'calibrate-main')
    if not ok:
        return ok, detail
    return True, 'aggregation-audit: opaque update payload flagged, passed=False'


def calibrate_param_augassign(rows):
    """#892 r12: ``def publish(report): report |= {'status': 'PASS'}`` writes a
    summary handed in as a parameter; the receiver gate only knew names bound to
    a dict literal, so the function was never enumerated (GLM boundary r11
    P1-1)."""
    return _unregistered_point(
        rows, 'publish', 'write-status',
        'aggregation-audit: parameter-receiver |= enumeration '
        'flagged, passed=False')


def calibrate_param_update(rows):
    """#892 r12: ``report.update(payload)`` on a parameter receiver, same
    enumeration hole as the ``|=`` spelling (GLM boundary r11 P1-1)."""
    return _unregistered_point(
        rows, 'publish', 'write-status',
        'aggregation-audit: parameter-receiver update enumeration '
        'flagged, passed=False')


def _unregistered_point(rows, function, marker, detail):
    """#892 r15: an unregistered point naming *function* and *marker*."""
    ok, reason = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, reason
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    failures = row.get('failures') or []
    if not any('unregistered aggregation point' in failure
               and failure.rstrip().endswith(f'{function} markers=[{marker!r}]')
               for failure in failures):
        return False, (f'failures={failures!r}, want the unregistered {function} '
                       f'point with markers=[{marker!r}]')
    return True, detail


def calibrate_augassign_cross_file_call(rows):
    """#892 r15: the ``|=`` payload call resolved by bare name, so a local
    ``run_all`` in another file inherited the registration (GLM boundary r14
    P1-1)."""
    return _unregistered_point(
        rows, 'publish', 'write-status',
        'aggregation-audit: cross-file |= payload enumeration flagged, passed=False')


def calibrate_exit_call_unregistered(rows):
    """#892 r15: an exit call is a status/exit computation of its own, so a
    point whose only evidence is ``sys.exit(...)`` must be enumerated (MiniMax
    sibling r14 P2-1)."""
    return _unregistered_point(
        rows, 'publish', 'exit-call-status',
        'aggregation-audit: exit-call enumeration flagged, passed=False')


def calibrate_tuple_status_target(rows):
    """#892 r15: a tuple target writes the same status slot as the plain
    subscript form (MiniMax simplify r14 P2-2)."""
    return _unregistered_point(
        rows, 'publish', 'assign-status',
        'aggregation-audit: tuple-target status enumeration flagged, passed=False')


def calibrate_attr_collision(rows):
    """#892 r16: an attribute head may not inherit this file's registration
    (GLM boundary r15 P1-1)."""
    ok, reason = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, reason
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    failures = row.get('failures') or []
    if not any('inline status/exit assignment helpers.cleanup_status(audit)' in failure
               for failure in failures):
        return False, (f'failures={failures!r}, want the attribute-callee status write '
                       f'flagged')
    return True, 'aggregation-audit: attribute-head callee no longer inherits (rel, name)'


def calibrate_bare_raise_exit(rows):
    """#892 r16: ``raise SystemExit(0)`` is an exit-code computation like
    ``sys.exit(0)`` (MiniMax sibling r15 P1-1)."""
    return _unregistered_point(
        rows, 'publish', 'raise-exit-status',
        'aggregation-audit: bare raise SystemExit enumerated, passed=False')


def calibrate_alias_sysexit(rows):
    """#892 r17: ``import sys as s; s.exit(0)`` is the same exit as
    ``sys.exit(0)`` (GLM boundary r16 P0-1)."""
    return _unregistered_point(
        rows, 'publish', 'exit-call-status',
        'aggregation-audit: aliased sys.exit enumerated, passed=False')


def calibrate_alias_system_exit(rows):
    """#892 r17: ``from builtins import SystemExit as SE; raise SE(0)`` is the
    same raise as ``raise SystemExit(0)`` (GLM boundary r16 P0-1)."""
    return _unregistered_point(
        rows, 'publish', 'raise-exit-status',
        'aggregation-audit: aliased SystemExit enumerated, passed=False')


def _calibrate_status_write_spelling(rows, needle):
    """#892 r17: a status write behind ``setattr`` / ``__setitem__``."""
    ok, reason = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, reason
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    failures = row.get('failures') or []
    if not any(needle in failure for failure in failures):
        return False, (f'failures={failures!r}, want the {needle!r} status write '
                       f'flagged')
    return True, f'aggregation-audit: {needle} status write flagged'


def calibrate_setattr_status(rows):
    """#892 r17: ``setattr(report, 'status', ...)`` writes the same slot as
    ``report['status'] = ...`` (GLM boundary r16 P1-1)."""
    return _calibrate_status_write_spelling(
        rows, "inline status/exit assignment setattr(report, 'status', 'PASS')")


def calibrate_dunder_setitem_status(rows):
    """#892 r17: ``report.__setitem__('status', ...)`` is the same slot under
    the dunder spelling (GLM boundary r16 P1-1)."""
    return _calibrate_status_write_spelling(
        rows, "inline status/exit assignment report.__setitem__('status', 'PASS')")


def calibrate_local_import_alias(rows):
    """#892 r18: ``import sys as s`` inside a function body binds the same
    exit as the module header does (GLM boundary r17 P0-1)."""
    return _unregistered_point(
        rows, 'publish', 'exit-call-status',
        'aggregation-audit: function-local import alias enumerated, passed=False')


def calibrate_module_alias_binding(rows):
    """#892 r18: a module-level ``bail = sys.exit`` binds the same exit as the
    import it copies (GLM sibling r17 P1-1)."""
    return _unregistered_point(
        rows, 'publish', 'exit-call-status',
        'aggregation-audit: module-level assignment alias enumerated, passed=False')


def calibrate_attr_status(rows):
    """#892 r18: ``report.status = 'PASS'`` writes the same slot as
    ``report['status'] = 'PASS'`` (MiniMax sibling r17 P1-1)."""
    return _calibrate_status_write_spelling(
        rows, "inline status/exit assignment 'PASS'")


def calibrate_dunder_setattr_status(rows):
    """#892 r18: ``report.__setattr__('status', ...)`` is the same slot under
    the dunder spelling (MiniMax sibling r17 P1-1)."""
    return _calibrate_status_write_spelling(
        rows, "inline status/exit assignment report.__setattr__('status', 'PASS')")


def calibrate_unbound_setitem_status(rows):
    """#892 r18: ``dict.__setitem__(report, 'status', ...)`` writes the slot
    through the unbound three-argument form (GLM sibling r17 P2-2)."""
    return _calibrate_status_write_spelling(
        rows, "inline status/exit assignment dict.__setitem__(report, 'status', 'PASS')")


def calibrate_builtins_setattr_status(rows):
    """#892 r18: ``builtins.setattr(report, 'status', ...)`` is the qualified
    spelling of the bare builtin (GLM sibling r17 P2-2)."""
    return _calibrate_status_write_spelling(
        rows, "inline status/exit assignment builtins.setattr(report, 'status', 'PASS')")


def _guard_failure(rows, needle, detail):
    """The #892 *failure* of ``_guard.py:_failure`` that contains *needle*."""
    ok, reason = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, reason
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, reason = _point_failure(row, 'scripts/gateway-parity/_guard.py',
                                '_failure', needle, 'guard-failure')
    if not ok:
        return ok, reason
    return True, detail


def calibrate_try_assign_alias(rows):
    """#892 r19: ``try: bail = sys.exit`` at module level binds the same exit
    as the unchecked assignment does (GLM boundary r18 P1-1)."""
    return _unregistered_point(
        rows, 'publish', 'exit-call-status',
        'aggregation-audit: try-guarded module assignment alias enumerated, passed=False')


def calibrate_attr_exit_code(rows):
    """#892 r19: ``return status, helpers.EXIT_OK`` hands back an exit code
    whose origin no local binding can prove (GLM boundary r18 P0-1)."""
    return _guard_failure(
        rows, 'inline exit code (attribute relay) helpers.EXIT_OK',
        'aggregation-audit: attribute exit arm flagged, passed=False')


def calibrate_subscript_exit_code(rows):
    """#892 r19: ``return status, CODES[policy]`` reads the code out of a
    container the pass never tracked (GLM boundary r18 P0-1)."""
    return _guard_failure(
        rows, 'inline exit code (subscript relay) CODES[policy]',
        'aggregation-audit: subscript exit arm flagged, passed=False')


def calibrate_attr_code_relay(rows):
    """#892 r19: the same attribute code relayed through a local name is the
    shape ``code = EXIT_OK; return code`` and may not pass where the direct
    attribute arm fails (GLM boundary r18 P0-1)."""
    return _guard_failure(
        rows, 'inline exit code (attribute relay) code',
        'aggregation-audit: attribute exit code relayed through a local, passed=False')


def calibrate_try_else_completes(rows):
    """#892 r19: a handler that falls off its own end completes the statement
    whatever the ``else`` branch does (MiniMax boundary r18 P0-1)."""
    return _guard_failure(
        rows, 'inline exit code _code(policy, common)',
        'aggregation-audit: completing try handler flagged, passed=False')


def calibrate_operator_setitem_status(rows):
    """#892 r18: ``operator.setitem(report, 'status', ...)`` is the qualified
    unbound spelling (GLM sibling r17 P2-2)."""
    return _calibrate_status_write_spelling(
        rows, "inline status/exit assignment operator.setitem(report, 'status', 'PASS')")


def calibrate_exit_call_attribute(rows):
    """#892 r20: ``sys.exit(helpers.EXIT_OK)`` hands the process an exit code
    whose origin no local binding proves; the r19 attribute rule reached the
    return arm only (GLM sibling r19 P1 / MiniMax boundary r19 P0)."""
    return _guard_failure(
        rows, 'inline exit code helpers.EXIT_OK',
        'aggregation-audit: attribute exit-call argument flagged, passed=False')


def calibrate_exit_call_subscript(rows):
    """#892 r20: ``raise SystemExit(CODES[policy])`` reads the code out of a
    container the pass never tracked (MiniMax boundary r19 P0)."""
    return _guard_failure(
        rows, 'inline exit code CODES[policy]',
        'aggregation-audit: subscript exit-call argument flagged, passed=False')


def calibrate_shadowed_common(rows):
    """#892 r20: a module that rebinds ``common`` to a stub makes every call
    spelled ``common.aggregate_status`` a local function again, so the file is
    not auditable (GLM boundary r19 P1-1)."""
    ok, reason = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, reason
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    needle = 'rebinds the shared aggregator module'
    if not any(needle in failure for failure in (row.get('failures') or [])):
        return False, (f'failures={row.get("failures")!r}, want the {needle!r} '
                       'failure')
    return True, 'aggregation-audit: shadowed common binding flagged, passed=False'


def calibrate_computed_status_key(rows):
    """#892 r21: a summary keyed ``{'sta' + 'tus': 'PA' + 'SS'}`` is the same
    dict as ``{'status': 'PASS'}`` at runtime; the r20 folding reached the
    value side only, so the key side never saw it (GLM sibling r20 P1-1)."""
    return _guard_failure(
        rows, "'PA' + 'SS'",
        'aggregation-audit: folded status key flagged, passed=False')


def calibrate_computed_status_key_write(rows):
    """#892 r21: ``report['sta' + 'tus'] = 'PA' + 'SS'`` writes the very slot
    the constant spelling does, with the key and the value both folded
    (GLM sibling r20 P1-1)."""
    return _guard_failure(
        rows, "'PA' + 'SS'",
        'aggregation-audit: folded-key status write flagged, passed=False')


def _must_call_shared(rows, function, detail):
    """#892 r21: a registered point whose shared calls all went through a
    locally rebound spelling must read as not shared."""
    ok, reason = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, reason
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    needle = f'{function} must call common.aggregate_status'
    if not any(needle in failure for failure in (row.get('failures') or [])):
        return False, (f'failures={row.get("failures")!r}, want the {needle!r} '
                       'failure')
    return True, detail


def calibrate_shadowed_common_local(rows):
    """#892 r21: rebinding ``common`` inside the function body -- over the
    parameter itself -- makes every ``common.aggregate_status`` in it a call
    to the stub (GLM sibling r20 P1-2 / boundary r20 P1-1)."""
    return _must_call_shared(
        rows, '_failure',
        'aggregation-audit: function-scope shadowed common flagged, passed=False')


def calibrate_shadowed_receiver_local(rows):
    """#892 r21: a locally rebound relay receiver (``md = <stub>`` then
    ``md.common.aggregate_status``) was taken on the spelling alone
    (GLM boundary r20 P1-1)."""
    return _must_call_shared(
        rows, 'summarize_results',
        'aggregation-audit: rebound shared relay flagged, passed=False')


def calibrate_shadowed_common_global(rows):
    """#892 r21: ``global common`` rebinds the module binding at runtime from
    inside a function, out of the module walk's sight."""
    return calibrate_shadowed_common(rows)


def calibrate_shadowed_receiver_global(rows):
    """#892 r23: the healthy relay binding ``md = load_session_domain()``
    replaced by a stub at module scope (through ``global md`` in ``main``)
    kept every ``md.common.aggregate_status`` in the file taken on the
    spelling alone (GLM boundary r22 P0-1)."""
    return _must_call_shared(
        rows, 'summarize_results',
        'aggregation-audit: module-scope stub receiver flagged, passed=False')


def calibrate_shadowed_receiver_poison_fn(rows):
    """#892 r23: an unrelated ``def _poison_md(): global md; md = <stub>``
    rebinds the module receiver at runtime, out of the module walk's sight
    (GLM boundary r22 P0-1)."""
    return _must_call_shared(
        rows, 'summarize_results',
        'aggregation-audit: receiver poisoned through global flagged, '
        'passed=False')


def calibrate_shadowed_receiver_same_fn(rows):
    """#892 r23: ``global md; md = <stub>`` inside the registered point itself
    -- the function-scope scan skipped global-declared receivers, so the relay
    stayed trusted (GLM boundary r22 P0-1)."""
    return _must_call_shared(
        rows, 'summarize_results',
        'aggregation-audit: in-function global receiver stub flagged, '
        'passed=False')


def calibrate_redefined_loader(rows):
    """#892 r23: ``def _load(name, path): return SimpleNamespace(...)`` after
    ``_load = _guard.load`` keeps the call spelling while returning a stub;
    the loader trust was callee-blind (GLM boundary r22 P1-2)."""
    return calibrate_shadowed_common(rows)


def _not_shared(rows, rel, function, detail):
    """#892 r25: a registered point in *rel* whose shared relay lost its proof."""
    ok, reason = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, reason
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    needle = (f'aggregation-audit: {rel}:{function} must call '
              'common.aggregate_status')
    if not any(needle in failure for failure in (row.get('failures') or [])):
        return False, (f'failures={row.get("failures")!r}, want the {needle!r} '
                       'failure')
    return True, detail


def _audit_needle(rows, needle, detail):
    """#892 fix-r26: an aggregation-audit failure line containing *needle*."""
    ok, reason = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, reason
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    if not any(needle in failure for failure in (row.get('failures') or [])):
        return False, (f'failures={row.get("failures")!r}, want the {needle!r} '
                       'failure')
    return True, detail


def calibrate_conditional_def(rows):
    """#892 r25: a same-named ``def`` inside a module-level ``if`` runs at
    import time but the binding graph only walked the top level (GLM boundary
    r24 P0-1)."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: conditional module-scope def flagged, passed=False')


def calibrate_decorated_source(rows):
    """#892 r25: a decorator replaces the function object at def time while the
    binding graph still saw one ``None`` binding (GLM concurrency r24 P0-1)."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: decorated proven source flagged, passed=False')


def calibrate_nested_def_rebind(rows):
    """#892 r25: a nested ``def`` shadows the imported proven source for the
    whole body (GLM boundary r24 P0-2)."""
    return _not_shared(
        rows, NEG_REL, 'main',
        'aggregation-audit: nested-def rebind flagged, passed=False')


def calibrate_walrus_rebind(rows):
    """#892 r25: ``(name := None)`` binds in the enclosing function scope with
    no ``Assign`` statement to record it (GLM boundary r24 P0-2)."""
    return _not_shared(
        rows, NEG_REL, 'main',
        'aggregation-audit: walrus rebind flagged, passed=False')


def calibrate_dynamic_globals_write(rows):
    """#892 r25: ``globals()['<registered>'] = <stub>`` moves a module binding
    behind the binding graph's back (GLM boundary r24 P1-4)."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: dynamic globals write flagged, passed=False')


def calibrate_sys_modules_poison(rows):
    """#892 r25: ``sys.modules['_guard'] = <stub>`` satisfies the get arm with a
    stub instead of the module the bootstrap published (GLM concurrency r24
    P1-3)."""
    return calibrate_shadowed_common(rows)


def calibrate_shadowed_global_loader(rows):
    """#892 r25: ``global common; common = _load(...)`` was accepted on the
    loader spelling even when the same function shadows ``_load`` (GLM sibling
    r24 P1-1)."""
    return calibrate_shadowed_common(rows)


def calibrate_module_walrus(rows):
    """#892 fix-r26: a module-scope walrus rebinds the proven source with no
    ``Assign`` statement (GLM concurrency r25 P0-1 / boundary r25 P0-2)."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: module-scope walrus rebind flagged, passed=False')


def calibrate_dynamic_receiver_write(rows):
    """#892 fix-r26: ``globals()['md'] = <stub>`` moves the relay receiver
    behind the binding graph's back (GLM boundary r25 P0-1)."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: dynamic receiver write flagged, passed=False')


def calibrate_setattr_shared_write(rows):
    """#892 fix-r26: ``setattr(md, <key>, <stub>)`` over a literal dict key
    replaces the shared attribute (GLM concurrency r25 P1-6)."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: setattr shared write flagged, passed=False')


def calibrate_exec_nonliteral(rows):
    """#892 fix-r26: ``exec(<non-literal>)`` can write any name in this module
    (GLM boundary r25 P1-3)."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: non-literal exec flagged, passed=False')


def calibrate_negative_process_redefine(rows):
    """#892 fix-r26: a second same-named def in the file that *defines* a
    cross-file proven source keeps every importer's trust (GLM sibling r25
    #2 / boundary r25 P1-5)."""
    return _audit_needle(
        rows, 'no longer resolves to exactly one undecorated definition',
        f'aggregation-audit: {NEGATIVE_PROCESS_REL} second definition '
        'flagged, passed=False')


def calibrate_decorated_guard_load(rows):
    """#892 fix-r26: a decorated ``_guard.load`` is no longer the sanctioned
    loader even though its ``[None]`` binding is unchanged (GLM sibling r25
    #5 / boundary r25 P1-7)."""
    return _audit_needle(
        rows, 'no longer defines exactly one undecorated load(...)',
        f'aggregation-audit: {GUARD_REL} decorated loader flagged, passed=False')


def calibrate_direct_attribute_write(rows):
    """#892 fix-r27: ``md.common = <stub>`` replaces the shared attribute with
    no ``setattr`` call (DeepSeek sibling r26 P1-4)."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: direct attribute write flagged, passed=False')


def calibrate_dict_mutation_keys(rows):
    """#892 fix-r27: a literal key dict mutated after binding is unbounded
    (GLM boundary r26 P0-2)."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: mutated key dict flagged, passed=False')


def calibrate_sys_modules_wildcard(rows):
    """#892 fix-r27: a non-literal ``sys.modules`` key pre-seeds the entry the
    bootstrap trusts (GLM boundary r26 P0-3)."""
    return _audit_needle(
        rows, f'{CONTROLS_REL} rebinds the shared aggregator module',
        f'aggregation-audit: {CONTROLS_REL} wildcard pre-seed flagged, '
        'passed=False')


def calibrate_single_definition_dynamic(rows):
    """#892 fix-r27: a literal ``exec`` rebinding a proven source is not the
    single ``def`` the registry accepted (DeepSeek concurrency r26 P1-8)."""
    return _audit_needle(
        rows, f'{NEGATIVE_PROCESS_REL}:committed_status_contract no longer',
        f'aggregation-audit: {NEGATIVE_PROCESS_REL} dynamic rebind flagged, '
        'passed=False')


def calibrate_dynamic_shared_name(rows):
    """#892 fix-r27: a literal ``exec`` rebinding the shared name withdraws it
    (GLM concurrency r26 P1-1)."""
    return _audit_needle(
        rows, f'{CONTROLS_REL} rebinds the shared aggregator module',
        f'aggregation-audit: {CONTROLS_REL} dynamic shared-name write flagged, '
        'passed=False')


def calibrate_builtin_alias_setattr(rows):
    """#892 fix-r27: ``from builtins import setattr as _sa`` is the same
    callable as the bare name (DeepSeek boundary r26 P1-2)."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: builtin-alias setattr flagged, passed=False')


def calibrate_vars_dict_receiver(rows):
    """#892 fix-r27: ``vars(md)['common'] = <stub>`` writes the receiver through
    its ``__dict__`` (GLM simplify r26 P1-2)."""
    return _audit_needle(
        rows, f'{CHECK_REL} rebinds the shared aggregator module',
        f'aggregation-audit: {CHECK_REL} vars() receiver write flagged, '
        'passed=False')


def calibrate_loop_key_rebind(rows):
    """#892 fix-r27: the ``for`` target that spells the key is reassigned in the
    loop body, so it no longer names the sequence's key (DeepSeek concurrency
    r26 P1-3)."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: loop key rebind flagged, passed=False')


def calibrate_definition_header_walrus(rows):
    """#892 fix-r27: a walrus in a definition header runs in the enclosing
    scope (GLM concurrency r26 P1-2)."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: definition-header walrus flagged, passed=False')


def calibrate_sys_modules_chain_poison(rows):
    """#892 fix-r27: a key pre-seeded from another pre-seeded key's read had to
    be poisoned through a fixpoint, not one pass (GLM concurrency r26 P1-1)."""
    return _audit_needle(
        rows, f'{CONTROLS_REL} rebinds the shared aggregator module',
        f'aggregation-audit: {CONTROLS_REL} chained pre-seed flagged, '
        'passed=False')


def calibrate_walrus_receiver_local(rows):
    """#892 fix-r27: a walrus inside the audited function rebinds the receiver
    for the whole body (GLM boundary r26 P2-1)."""
    return _not_shared(
        rows, CHECK_REL, 'summarize_results',
        'aggregation-audit: function-local walrus receiver flagged, passed=False')


def calibrate_computed_status_write(rows):
    """#892 r20: ``report['status'] = 'PA' + 'SS'`` writes the clean literal
    without ever being a ``Constant`` node (GLM boundary r19 P1-2)."""
    return _guard_failure(
        rows, "inline status/exit assignment {'status': 'PA' + 'SS'}",
        'aggregation-audit: computed clean status write flagged, passed=False')


def calibrate_local_exit_alias_chain(rows):
    """#892 r20: ``bail = sys.exit; other = bail; other(0)`` is the same exit
    call one hop further out; the function-level alias pass resolved the chain
    only when the source order happened to be favourable (MiniMax boundary r19
    P1)."""
    return _unregistered_point(
        rows, 'publish', 'exit-call-status',
        'aggregation-audit: two-hop local exit alias enumerated, passed=False')


def calibrate_delegated_attribute(rows):
    """#892 r10: ``return helpers.exit_code(report)`` hides the exit code behind
    an attribute callee the audit never followed (GLM boundary r9 P1-2)."""
    return _controls_main_needle(
        rows, 'inline exit code helpers.exit_code(report)',
        'aggregation-audit: attribute delegation flagged, passed=False')


def calibrate_update_unregistered(rows):
    """#892 r10: an unregistered function whose only status evidence is an
    opaque ``update()`` payload must still be enumerated (MiniMax boundary r9
    P2-3)."""
    ok, reason = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, reason
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    failures = row.get('failures') or []
    if not any('unregistered aggregation point' in failure
               and failure.rstrip().endswith(':publish markers=[\'write-status\']')
               for failure in failures):
        return False, (f'failures={failures!r}, want an unregistered point named '
                       'publish carrying the write-status marker')
    return True, 'aggregation-audit: opaque update enumeration flagged, passed=False'


def calibrate_walrus_exit(rows):
    """#892 r10: the walrus exit-code relay the r9 round proved only with an
    in-file probe (MiniMax sibling r9 P2-2)."""
    return _controls_main_needle(
        rows, '(int() relay) code',
        'aggregation-audit: walrus exit code flagged, passed=False')


def calibrate_early_return_branch(rows):
    """#892 r5: a zero branch that returns before the shared rebind is inline
    even though a later sibling overwrites the name (MiniMax boundary P0-1)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _point_failure(
        row, 'scripts/gateway-parity/session_domain.py', 'main',
        'inline exit code (literal relay) code', 'calibrate-main')
    if not ok:
        return ok, detail
    return True, ('aggregation-audit: early return inside a zero branch flagged, '
                  'passed=False')


def calibrate_two_hop_zero(rows):
    """#892 r5: ``rc = 0; relay = rc; return relay`` is still an optimistic exit
    (GLM concurrency P1-1)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _point_failure(
        row, 'scripts/gateway-parity/session_domain.py', 'main',
        'inline exit code (literal relay) code', 'calibrate-main')
    if not ok:
        return ok, detail
    return True, 'aggregation-audit: two-hop exit code relay flagged, passed=False'


def calibrate_int_call_code(rows):
    """#892 r5: ``code = int(all(...)); return code`` computes the code inline
    (GLM concurrency P1-1)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _point_failure(
        row, 'scripts/gateway-parity/session_domain.py', 'main',
        'inline exit code (int() relay) code', 'calibrate-main')
    if not ok:
        return ok, detail
    return True, 'aggregation-audit: int() exit code in a local flagged, passed=False'


def calibrate_dict_code_zero(rows):
    """#892 r5: ``holder = {'code': 0}; return holder['code']`` reads a zero back
    out of a local dict (GLM concurrency P1-1)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _point_failure(
        row, 'scripts/gateway-parity/session_domain.py', 'main',
        "inline exit code holder['code']", 'calibrate-main')
    if not ok:
        return ok, detail
    return True, ('aggregation-audit: zero read back from a local dict flagged, '
                  'passed=False')


def calibrate_nested_def_zero(rows):
    """#892 r5: a nested helper that returns a constant zero is an optimistic
    exit (MiniMax boundary P1-4)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _point_failure(
        row, 'scripts/gateway-parity/session_domain.py', 'main',
        'inline exit code _exit_code()', 'calibrate-main')
    if not ok:
        return ok, detail
    return True, 'aggregation-audit: nested zero helper flagged, passed=False'


def calibrate_sys_exit_zero(rows):
    """#892 r5: ``sys.exit(0)`` is the same optimistic exit as
    ``raise SystemExit(0)`` (MiniMax sibling P1-1)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _point_failure(
        row, 'scripts/gateway-parity/session_domain.py', 'main',
        'inline optimistic exit (sys.exit(0))', 'calibrate-main')
    if not ok:
        return ok, detail
    return True, 'aggregation-audit: sys.exit(0) flagged, passed=False'


def calibrate_walrus_status(rows):
    """#892 r5: a walrus write carries the clean literal like any other value
    (MiniMax sibling P1-2)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _point_failure(
        row, 'scripts/gateway-parity/session_domain.py', 'main',
        "inline status/exit assignment (status := 'PASS')", 'calibrate-main')
    if not ok:
        return ok, detail
    return True, 'aggregation-audit: walrus status write flagged, passed=False'


def calibrate_rebuilt_summary(rows):
    """#892 r5: a summary rebuilt from an existing one is not a leaf verdict, so
    its clean literal is judged even though the name feeds the shared call
    (GLM boundary P1-3)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _point_failure(
        row, 'scripts/gateway-parity/session_domain.py', 'main',
        "inline status/exit assignment {**report, 'status': 'PASS'}",
        'calibrate-main')
    if not ok:
        return ok, detail
    return True, 'aggregation-audit: rebuilt summary flagged, passed=False'


def calibrate_verdict_write(rows):
    """#892 r5: ``report['verdict'] = 'PASS'`` is the same status key family as
    ``report['status']`` (MiniMax boundary P1-3)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _point_failure(
        row, 'scripts/gateway-parity/session_domain.py', 'main',
        "inline status/exit assignment 'PASS'", 'calibrate-main')
    if not ok:
        return ok, detail
    return True, 'aggregation-audit: verdict status write flagged, passed=False'


def calibrate_status_update_kwarg(rows):
    """#892 r4: ``report.update(status='PASS')`` passes the slot as a keyword
    argument (DeepSeek boundary P0-2)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _point_failure(
        row, 'scripts/gateway-parity/session_domain.py', 'main',
        "report.update(status='PASS')", 'calibrate-main')
    if not ok:
        return ok, detail
    return True, ('aggregation-audit: update() keyword status write flagged, '
                  'passed=False')


def retirement_duplicate_status_key(rows):
    """#892 r3: ``{'status': 'BLOCKED', 'status': 'PASS'}`` reads as clean at
    runtime but the first key wins in the audit (DeepSeek boundary P2)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    ok, detail = _point_failure(
        row, 'scripts/gateway-parity/session_domain.py', '_retirement_report',
        "status slot 'PASS'", 'retirement-report')
    if not ok:
        return ok, detail
    return True, ('aggregation-audit: duplicate status key flagged, '
                  'passed=False')


def controls_nested_same_name(rows):
    """#892 r3: a nested ``def main`` must not shadow or duplicate the
    registered point name (MiniMax/DeepSeek boundary P2)."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    failures = row.get('failures') or []
    if not any(failure.startswith(
            '#892 aggregation-audit: duplicate aggregation point name '
            'scripts/gateway-parity/controls/session_domain.py:main')
            for failure in failures):
        return False, f'failures={failures!r}, want the duplicate point-name failure'
    return True, ('aggregation-audit: nested same-name def flagged, '
                  'passed=False')


# (name, source, old, new).  Every old block must occur once.  Per-mutant
# judge metadata (mode, judge, uses_launcher, allowed_probe_errors) lives in
# JUDGES below: #820's anchor selfcheck reads literal replacement tables only,
# so callables stay out of MUTATIONS.
MUTATIONS = [
    ('kernel-source-lease-gate-removed', CHECK,
     '    if proof is None:\n        return kernel_control_report(None,None,reason)',
     '    if False:\n        return kernel_control_report(None,None,reason)'),
    ('kernel-source-lease-cleanup-pending-guard-removed', CHECK,
     "    if owner.get('cleanupPending') is not True:",
     '    if False:'),
    ('kernel-source-lease-token-guard-removed', CHECK,
     "    if not isinstance(owner.get('token'),str) or not owner['token']:",
     '    if False:'),
    ('kernel-source-lease-timestamp-guard-removed', CHECK,
     "    if not 0<=time.time()-owner['timestamp']<=md.common.MAX_AGE:",
     '    if False:'),
    ('kernel-source-lease-flock-guard-removed', CHECK,
     '        if not any(kernel_flock_match(line,parent,identity) for line in kernel_flock_rows()):',
     '        if False:'),
    ('kernel-result-passed-forced', CHECK,
     "            'passed': status=='PASS','report':report,'reason':reason}",
     "            'passed': True,'report':report,'reason':reason}"),

    ('empty-eligible-guard-removed', NEGATIVE,
     '    if not mutation_inventory_matches(sys.platform,eligible):\n        common=committed_status_contract(ROOT)',
     '    if False:\n        common=committed_status_contract(ROOT)'),

    ('linux-initial-identity-gate-removed', LINUX_SOURCE,
     "    if held._state() != 'EXITED':\n        result.update(status=not_verified, reason='Linux leader identity was lost before cleanup proof')",
     "    if False:\n        result.update(status=not_verified, reason='Linux leader identity was lost before cleanup proof')"),
    ('linux-pre-reap-identity-gate-removed', LINUX_SOURCE,
     "        if held._state() != 'EXITED':\n            result.update(status=not_verified, reason='Linux leader identity was lost before reap')",
     "        if False:\n            result.update(status=not_verified, reason='Linux leader identity was lost before reap')"),
    ('linux-refusal-erases-finding', LINUX_SOURCE,
     'result.update(status=not_empty, signalRefused=str(detail),',
     'result.update(status=not_verified, signalRefused=str(detail),'),

    ('linux-empty-group-probe-removed', LINUX_SOURCE,
     'status=not_empty if finding else verified,',
     'status=not_empty,'),
    ('linux-sigterm-removed', LINUX_SOURCE,
     '            for sig in (signal.SIGTERM, signal.SIGKILL):',
     '            for sig in (signal.SIGKILL,):'),
    ('linux-sigkill-escalation-removed', LINUX_SOURCE,
     '            for sig in (signal.SIGTERM, signal.SIGKILL):',
     '            for sig in (signal.SIGTERM,):'),
    ('linux-positive-finding-dropped-on-denial', LINUX_SOURCE,
     "status=not_empty if finding or state == 'alive' else blocked,",
     "status=not_empty if state == 'alive' else verified,"),
    ('linux-unknown-error-means-empty', LINUX_SOURCE,
     "state, detail = 'unknown', f'{type(error).__name__}: {error}'",
     "state, detail = 'unknown', f'{type(error).__name__}: {error}'\n                result.update(status=verified, kernelGroupProbe='unknown');return result"),
    ('linux-controls-summary-forced-pass', LINUX_CHECK,
     "    status, exit_code = common.aggregate_status('PASS' if healthy else 'FAIL')",
     "    status, exit_code = 'PASS', 0"),

    ('empty-group-probe-removed', SOURCE,
     "    return 'empty', (f'killpg0=EPERM with the unreaped zombie leader present '\n"
     "                     f'and an empty live-member census: {detail}')",
     "    return 'alive', 'mutant: empty-group probe removed'"),
    ('cleanup-status-write-removed', SOURCE,
     "        audit['processTreeCleanup'] = audit['processGroupProbe']['status']",
     "        audit['processTreeCleanup'] = CLEANUP_VERIFIED"),
    ('top-level-mapping-relaxed', SOURCE,
     "    return ('PASS' if audit.get('processTreeCleanup') == CLEANUP_VERIFIED\n"
     "            else CLEANUP_UNVERIFIED_STATUS)",
     "    return 'PASS'"),
    ('sigterm-removed', SOURCE,
     "    for sig in (signal.SIGTERM, signal.SIGKILL):",
     "    for sig in (signal.SIGKILL,):"),
    ('blocked-branch-removed', SOURCE,
     "    if outcome == 'error':\n        return 'blocked', detail",
     "    if outcome == 'error':\n        raise  # mutant: remove the BLOCKED branch"),
    ('sigkill-escalation-removed', SOURCE,
     "    for sig in (signal.SIGTERM, signal.SIGKILL):",
     "    for sig in (signal.SIGTERM,):"),
    ('top-level-wiring-relaxed', SOURCE,
     "                overall = cleanup_status(audit)",
     "                overall = 'PASS'"),
    ('eperm-census-ignored', SOURCE,
     "    live = live_group_members(held.pid)",
     "    live = set()  # mutant: ignore the independent live-member census"),
    ('unavailable-census-as-empty', SOURCE,
     "        return 'blocked', ('killpg0=EPERM; independent group census unavailable '\n"
     "                           f'or incomplete: {detail}')",
     "        live = set()  # mutant: treat an unavailable census as empty"),
    ('reap-guard-removed', SOURCE,
     "        return self._signal(sig, group=False, states=('RUNNING',))",
     "        return self._signal(sig, group=False, states=('RUNNING', 'EXITED', 'REAPED'))"),
    ('snapshot-race-accepted', SOURCE,
     "        if first != second:\n"
     "            # Membership changed between complete snapshots: a descendant may\n"
     "            # exist that neither snapshot can vouch for.\n"
     "            continue",
     "        if first != second:\n            return first  # mutant: accept the first snapshot"),
    ('unstable-census-accepted', SOURCE,
     "        if first != second:\n"
     "            # Membership changed between complete snapshots: a descendant may\n"
     "            # exist that neither snapshot can vouch for.\n"
     "            continue",
     "        if first != second:\n            return second  # mutant: accept a changing census"),
    ('truncation-accepted', SOURCE,
     "            if written < capacity and written % 4 == 0:",
     "            if written <= capacity:  # mutant: accept a full (truncated) buffer"),
    ('enumeration-failure-empty', SOURCE,
     "        if first is None:\n            continue",
     "        if first is None:\n            return set()  # mutant: enumeration failure == empty"),
    ('signal-gate-removed', SOURCE,
     "        state = self._state()\n        if state not in states:\n            return 'refused', state",
     "        state = self._state()\n        # mutant: signal without proving ownership"),
    ('pid-blocked-as-gone', SOURCE,
     "        except ProcessLookupError:\n            return 'missing'\n"
     "        except OSError:\n            return 'blocked'",
     "        except ProcessLookupError:\n            return 'missing'\n"
     "        except OSError:\n            return 'missing'  # mutant: blocked == gone"),
    ('linux-visible-census-trusted', LINUX_SOURCE,
     "    finding = bool(members)\n",
     "    finding = bool(members)\n    if not members:\n        result.update(status=verified)\n        return result  # mutant: empty visible /proc is trusted\n"),
    ('linux-eperm-means-empty', LINUX_SOURCE,
     "                state, detail = 'denied', str(error)",
     "                result.update(status=verified)\n                return result  # mutant: EPERM is treated as emptiness"),
    ('linux-no-esrch-needed', LINUX_SOURCE,
     "                state, detail = 'alive', 'kernel still observes the process group'",
     "                result.update(status=verified)\n                return result  # mutant: a live group is accepted"),
    ('batch-status-relaxed', CONTROLS_SOURCE,
     "    cleanup = cleanup_controls(output)\n"
     "    status, _ = common.aggregate_status(\n"
     "        'PASS' if all(r['passed'] for r in results) else 'FAIL',\n"
     "        'PASS_WITH_SKIPS' if not_run else None,\n"
     "        cleanup['status'])\n"
     "    report = {'status': status,\n"
     "              'scope': 'apparatus mutation tests only', 'cases': results,\n"
     "              'notRun': [{'case': case, 'reason': 'requires real calibration --source'} for case in not_run]}\n"
     "    report['cleanup'] = cleanup\n"
     "    report['cleanupSkipped'] = cleanup.get('skipped', [])",
     "    cleanup = cleanup_controls(output)\n"
     "    report = {'status': 'PASS' if all(r['passed'] for r in results) else 'FAIL',\n"
     "              'scope': 'apparatus mutation tests only', 'cases': results,\n"
     "              'notRun': [{'case': case, 'reason': 'requires real calibration --source'} for case in not_run]}\n"
     "    report['cleanup'] = cleanup\n"
     "    report['cleanupSkipped'] = cleanup.get('skipped', [])\n"
     "    if cleanup['status'] == 'FAIL':\n"
     "        report['status'] = 'FAIL'"),
    # The anchor is the whole pessimistic branch, because the mutant replaces
    # it with a naive ``all(row.get('passed'))`` summary.  r20 changed the
    # condition to ``is not True`` (MiniMax boundary r19 P2-3); the anchor moved
    # with it, and ``anchor_selfcheck`` is what reports the drift when it does.
    ('fixture-summary-relaxed', CHECK,
     "    if (malformed or not executed\n"
     "            or any(row.get('passed') is not True for row in executed)):\n"
     "        # ``is False`` let a malformed ``passed=0`` / ``passed='no'`` count as\n"
     "        # a non-failing row and the run report PASS; only a literal ``True``\n"
     "        # passes (MiniMax boundary r19 P2-3).\n"
     "        return md.common.aggregate_status('FAIL')[0], skipped\n"
     "    status, _ = md.common.aggregate_status(\n"
     "        'PASS', 'PASS_WITH_SKIPS' if skipped else None)\n"
     "    return status, skipped",
     "    if not results or not all(row.get('passed') for row in results):\n"
     "        return 'FAIL', []\n"
     "    return 'PASS', []"),
    ('calibrate-controls-skip-accepted', SOURCE,
     "                    if controls_status == 'PASS_WITH_SKIPS':\n"
     "                        # A skipped positive control is not proof of the batch.\n"
     "                        # Keep the skipped names visible and never report PASS:\n"
     "                        # BLOCKED / exit 2 is the apparatus' non-PASS convention.\n"
     "                        report['controlsSkipped'] = (\n"
     "                            report['e1Controls'].get('cleanupSkipped', [])\n"
     "                            + [row['case'] for row in report['e1Controls'].get('notRun', [])])\n"
     "                        report['failure'] = {\n"
     "                            'rule': 'CONTROLS_SKIPPED',\n"
     "                            'detail': 'E1 controls skipped platform-specific positives; not PASS'}\n"
     "                        overall = 'BLOCKED'",
     "                    # mutant: accept a skipped controls batch as PASS"),
    ('aggregation-exit-bypassed', CONTROLS_SOURCE,
     "        return common.aggregate_status(report['status'])[1]",
     "        return int(report['status'] != 'PASS')"),
    ('summary-dict-bypass', CONTROLS_SOURCE,
     "    cleanup = cleanup_controls(output)\n"
     "    status, _ = common.aggregate_status(\n"
     "        'PASS' if all(r['passed'] for r in results) else 'FAIL',\n"
     "        'PASS_WITH_SKIPS' if not_run else None,\n"
     "        cleanup['status'])\n"
     "    report = {'status': status,\n",
     "    cleanup = cleanup_controls(output)\n"
     "    status, _ = common.aggregate_status('PASS', cleanup['status'])\n"
     "    def inline_batch_summary(results, not_run, cleanup):\n"
     "        # mutant #892: a new summary point bypassing the shared function\n"
     "        return {'status': 'PASS' if all(r['passed'] for r in results) else 'FAIL'}\n"
     "    report = inline_batch_summary(results, not_run, cleanup)\n"
     "    report = {'status': report['status'],\n"),
    ('aggregation-branch-exit-inline', CONTROLS_SOURCE,
     "        print(json.dumps(report, ensure_ascii=False))\n"
     "        return common.aggregate_status(report['status'])[1]\n",
     "        print(json.dumps(report, ensure_ascii=False))\n"
     "        return 0 if report['status'] == 'PASS' else 1\n"),
    ('summary-name-bypass', CONTROLS_SOURCE,
     "    report = {'status': status,\n"
     "              'scope': 'apparatus mutation tests only', 'cases': results,\n",
     "    report = {'status': 'PASS',\n"
     "              'scope': 'apparatus mutation tests only', 'cases': results,\n"),
    ('bare-exit-zero', CONTROLS_SOURCE,
     "        print(json.dumps(report, ensure_ascii=False))\n"
     "        return common.aggregate_status(report['status'])[1]\n",
     "        print(json.dumps(report, ensure_ascii=False))\n"
     "        return 0\n"),
    ('status-slot-relayed', CONTROLS_SOURCE,
     "    report = {'status': status,\n"
     "              'scope': 'apparatus mutation tests only', 'cases': results,\n",
     "    status = 'PASS' if all(r['passed'] for r in results) else 'FAIL'\n"
     "    report = {'status': status,\n"
     "              'scope': 'apparatus mutation tests only', 'cases': results,\n"),
    ('system-exit-zero-bypass', CONTROLS_SOURCE,
     "        print(json.dumps(report, ensure_ascii=False))\n"
     "        return common.aggregate_status(report['status'])[1]\n",
     "        print(json.dumps(report, ensure_ascii=False))\n"
     "        raise SystemExit(0)\n"),
    ('bare-return-exit', CONTROLS_SOURCE,
     "        print(json.dumps(report, ensure_ascii=False))\n"
     "        return common.aggregate_status(report['status'])[1]\n",
     "        print(json.dumps(report, ensure_ascii=False))\n"
     "        return\n"),
    ('conditional-arm-exit', CONTROLS_SOURCE,
     "        print(json.dumps(report, ensure_ascii=False))\n"
     "        return common.aggregate_status(report['status'])[1]\n",
     "        print(json.dumps(report, ensure_ascii=False))\n"
     "        return int(report.get('special')) if report.get('special') else common.aggregate_status(report['status'])[1]\n"),
    ('calibrate-bare-zero', SOURCE,
     "            return code\n",
     "            return 0\n"),
    ('calibrate-status-write-pass', SOURCE,
     "                report['status'] = 'BLOCKED' if isinstance(error, BLOCKED_APPARATUS_ERRORS) else 'FAIL'\n",
     "                report['status'] = 'PASS'\n"),
    ('restored-skip-accepted', NEGATIVE,
     "            top_status, exit_code = common.aggregate_status(\n"
     "                *(MUTATION_STATUS.get(entry['verdict'], 'FAIL') for entry in summary),\n"
     "                restored_summary.get('status') if restored_ok else 'FAIL')",
     "            top_status, exit_code = common.aggregate_status(\n"
     "                *(MUTATION_STATUS.get(entry['verdict'], 'FAIL') for entry in summary))"),
    # #892 fix-r3: the eight false-pass vectors the r2 reviews found.
    ('controls-return-none', CONTROLS_SOURCE,
     "        print(json.dumps(report, ensure_ascii=False))\n"
     "        return common.aggregate_status(report['status'])[1]\n",
     "        print(json.dumps(report, ensure_ascii=False))\n"
     "        return None\n"),
    ('controls-system-exit-none', CONTROLS_SOURCE,
     "        print(json.dumps(report, ensure_ascii=False))\n"
     "        return common.aggregate_status(report['status'])[1]\n",
     "        print(json.dumps(report, ensure_ascii=False))\n"
     "        raise SystemExit(None)\n"),
    ('calibrate-exit-code-relayed', SOURCE,
     "                    _, code = common.aggregate_status(report['status'])\n",
     "                    code = 0 if report['status'] == 'PASS' else 1\n"),
    ('controls-status-slot-subscript', CONTROLS_SOURCE,
     "    report = {'status': status,\n"
     "              'scope': 'apparatus mutation tests only', 'cases': results,\n",
     "    local = {'status': 'PASS'}\n"
     "    report = {'status': local['status'],\n"
     "              'scope': 'apparatus mutation tests only', 'cases': results,\n"),
    ('controls-aggregate-lookalike', CONTROLS_SOURCE,
     "    status, _ = common.aggregate_status(\n"
     "        'PASS' if all(r['passed'] for r in results) else 'FAIL',\n"
     "        'PASS_WITH_SKIPS' if not_run else None,\n"
     "        cleanup['status'])\n",
     "    def aggregate_status(*children):\n"
     "        return 'PASS', 0\n"
     "    status, _ = aggregate_status(\n"
     "        'PASS' if all(r['passed'] for r in results) else 'FAIL',\n"
     "        'PASS_WITH_SKIPS' if not_run else None,\n"
     "        cleanup['status'])\n"),
    ('calibrate-status-update', SOURCE,
     "                report['status'] = 'BLOCKED' if isinstance(error, BLOCKED_APPARATUS_ERRORS) else 'FAIL'\n",
     "                report.update({'status': 'PASS'})\n"),
    ('retirement-duplicate-status-key', SOURCE,
     "        'status': overall,\n",
     "        'status': 'BLOCKED', 'status': 'PASS',\n"),
    ('controls-nested-same-name', CONTROLS_SOURCE,
     "    report['cleanup'] = cleanup\n",
     "    def main():\n"
     "        return common.aggregate_status('PASS')\n"
     "    report['cleanup'] = cleanup\n"),
    # #892 fix-r4: the three false-pass vectors the r3 reviews found.
    ('calibrate-shared-excuse', SOURCE,
     "                    _, code = common.aggregate_status(report['status'])\n",
     "                    code = 0\n"),
    ('calibrate-status-setdefault', SOURCE,
     "                report['status'] = 'BLOCKED' if isinstance(error, BLOCKED_APPARATUS_ERRORS) else 'FAIL'\n",
     "                report.setdefault('status', 'PASS')\n"),
    ('calibrate-early-return-branch', SOURCE,
     A_EXIT,
     "                    if report.get('special'):\n"
     "                        code = 0\n"
     "                        return code\n" + A_EXIT),
    ('calibrate-two-hop-zero', SOURCE,
     A_EXIT,
     "                    if report.get('special'):\n"
     "                        rc = 0\n"
     "                        relay = rc\n"
     "                        return relay\n" + A_EXIT),
    ('calibrate-int-call-code', SOURCE,
     A_EXIT,
     "                    if report.get('special'):\n"
     "                        code = int(all(bool(row.get('passed')) "
     "for row in report.get('cells') or ()))\n"
     "                        return code\n" + A_EXIT),
    ('calibrate-dict-code-zero', SOURCE,
     A_EXIT,
     "                    if report.get('special'):\n"
     "                        holder = {'code': 0}\n"
     "                        return holder['code']\n" + A_EXIT),
    ('calibrate-nested-def-zero', SOURCE,
     A_EXIT,
     "                    if report.get('special'):\n"
     "                        def _exit_code():\n"
     "                            return 0\n"
     "                        return _exit_code()\n" + A_EXIT),
    ('calibrate-verdict-write', SOURCE,
     "                report['status'] = 'BLOCKED' if isinstance(error, "
     "BLOCKED_APPARATUS_ERRORS) else 'FAIL'\n",
     "                report['verdict'] = 'PASS'\n"),
    ('calibrate-sys-exit-zero', SOURCE,
     A_EXIT,
     "                    if report.get('special'):\n"
     "                        sys.exit(0)\n" + A_EXIT),
    ('calibrate-walrus-status', SOURCE,
     B_STATUS,
     "                report['status'] = (status := 'PASS')\n"),
    ('calibrate-rebuilt-summary', SOURCE,
     B_STATUS,
     "                report = {**report, 'status': 'PASS'}\n"),
    # #892 fix-r6: exit-0 spellings the r5 reviews found outside the Return arm.
    ('calibrate-boolean-exit', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            ok = bool(report.get('cells'))\n"
     "            return not ok\n" + CT_EXIT),
    ('calibrate-sys-exit-relay', CONTROLS_SOURCE,
     CT_EXIT,
     "        holder = {'code': 0}\n"
     "        if report.get('special'):\n"
     "            sys.exit(holder['code'])\n" + CT_EXIT),
    ('calibrate-system-exit-bare', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            raise SystemExit\n" + CT_EXIT),
    ('calibrate-delegated-chain', CONTROLS_SOURCE,
     CT_EXIT,
     "        def _exit_a(rep):\n"
     "            return _exit_b(rep)\n\n"
     "        def _exit_b(rep):\n"
     "            return 0 if rep.get('ok') else 1\n\n"
     "        return _exit_a(report)\n"),
    ('calibrate-container-slot', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            report['status'] = upstream['status']\n" + CT_EXIT),
    ('calibrate-alias-cycle', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            status, code = code, status\n"
     "            return {'status': status}, code\n" + CT_EXIT),
    # #892 fix-r6: the hardcoded clean status can also sit in the shared call's
    # own argument list (MiniMax concurrency r5 P1-1).
    ('calibrate-clean-argument', CONTROLS_SOURCE,
     "        'PASS' if all(r['passed'] for r in results) else 'FAIL',\n",
     "        'PASS',\n"),
    ('calibrate-verdict-dict', CONTROLS_SOURCE,
     CT_MAIN,
     "def publish(rows):\n"
     "    payload = {'verdict': 'PASS', 'rows': rows}\n"
     "    print(json.dumps(payload))\n"
     "    return 0\n\n\n" + CT_MAIN),
    # #892 fix-r7: the return-path shapes that still walked through in r6.
    ('calibrate-conditional-summary', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            return {'status': 'PASS'} if report.get('cells') else report\n"
     + CT_EXIT),
    ('calibrate-update-expansion', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            report.update(**extra)\n" + CT_EXIT),
    ('calibrate-exit-conditional', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            sys.exit(0 if report.get('cells') else 1)\n" + CT_EXIT),
    ('calibrate-return-exit-call', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            return sys.exit(0)\n" + CT_EXIT),
    # #892 fix-r8: three more write/exit spellings from the r6 reviews.
    ('calibrate-ann-rebuilt', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            report: dict = {**report, 'status': 'PASS'}\n" + CT_EXIT),
    ('calibrate-bare-verdict', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            verdict = 'PASS'\n"
     "            print(verdict)\n" + CT_EXIT),
    ('calibrate-bool-exit', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            return bool(report.get('cells'))\n" + CT_EXIT),
    # #892 fix-r9: the relay and write spellings the r7 reviews found.
    ('calibrate-bool-relay', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            code = bool(report.get('cells'))\n"
     "            return code\n" + CT_EXIT),
    ('calibrate-unbound-exit-name', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            return EXIT_CODE\n" + CT_EXIT),
    ('calibrate-update-positional', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            report.update(extra)\n" + CT_EXIT),
    ('calibrate-augassign-update', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            report |= extra\n" + CT_EXIT),
    ('calibrate-tuple-verdict', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            verdict, code = 'PASS', 0\n"
     "            print(code)\n" + CT_EXIT),
    ('calibrate-cleanup-word', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            report['status'] = 'PASS_WITH_CLEANUP_UNVERIFIED'\n" + CT_EXIT),
    # #892 fix-r10: the r9-round P0/P1 shapes.
    ('calibrate-unbound-exit-relay', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            code = EXIT_OK\n"
     "            return code\n" + CT_EXIT),
    ('calibrate-no-exit-path', CONTROLS_SOURCE,
     "    save(output / 'results.json', report)\n"
     "    return report\n",
     "    save(output / 'results.json', report)\n"
     "    print(json.dumps(report, ensure_ascii=False))\n"),
    ('calibrate-delegated-attribute', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            return helpers.exit_code(report)\n" + CT_EXIT),
    ('calibrate-update-unregistered', CONTROLS_SOURCE,
     "def run_all(output, source=None):",
     "def publish(rows):\n"
     "    report = {'cases': rows}\n"
     "    report.update(extra)\n"
     "    return report\n"
     "def run_all(output, source=None):"),
    ('calibrate-walrus-exit', CONTROLS_SOURCE,
     CT_EXIT,
     "        if (code := int(bool(report.get('cells')))):\n"
     "            return code\n" + CT_EXIT),
    ('calibrate-status-update-kwarg', SOURCE,
     "                report['status'] = 'BLOCKED' if isinstance(error, BLOCKED_APPARATUS_ERRORS) else 'FAIL'\n",
     "                report.update(status='PASS')\n"),
    # #892 fix-r11: the r10-round P0/P1 shapes.
    ('calibrate-helper-bare-return', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            return check_case(args.case, args.output, args.source)\n"
     + CT_EXIT),
    ('calibrate-helper-exit-call', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            def _bail():\n"
     "                raise SystemExit(0)\n"
     "            return _bail()\n" + CT_EXIT),
    ('calibrate-augassign-unregistered', CONTROLS_SOURCE,
     "def run_all(output, source=None):",
     "def publish(rows):\n"
     "    report = {'cases': rows}\n"
     "    report |= {'status': 'PASS'}\n"
     "    return report\n"
     "def run_all(output, source=None):"),
    # #892 fix-r12: the r11-round P0/P1 shapes.
    ('calibrate-opaque-clean-write', SOURCE,
     "            report['status'] = overall\n",
     "            report['status'] = _clean()\n"),
    ('calibrate-opaque-update-payload', SOURCE,
     "                    report.update(deadline_report(args.native_capture))\n",
     "                    report.update(_payload(report))\n"),
    ('calibrate-param-augassign', CONTROLS_SOURCE,
     "def run_all(output, source=None):",
     "def publish(report):\n"
     "    report |= {'status': 'PASS'}\n"
     "    return report\n"
     "def run_all(output, source=None):"),
    ('calibrate-param-update', CONTROLS_SOURCE,
     "def run_all(output, source=None):",
     "def publish(report, payload):\n"
     "    report.update(payload)\n"
     "    return report\n"
     "def run_all(output, source=None):"),
    # #892 fix-r13: the r12-round P0/P1 shapes.
    ('calibrate-helper-local-relay', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            def _relay():\n"
     "                code = 0\n"
     "                return code\n"
     "            return _relay()\n" + CT_EXIT),
    ('calibrate-helper-bare-sysexit', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            def _bail():\n"
     "                raise SystemExit\n"
     "            return _bail()\n" + CT_EXIT),
    ('calibrate-attr-head-callee', SOURCE,
     "            report['status'] = overall\n",
     "            report['status'] = helpers._clean()\n"),
    ('calibrate-mixed-branch-helper', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            def _code(rep):\n"
     "                if rep.get('special'):\n"
     "                    raise ValueError('x')\n"
     "            return _code(report)\n" + CT_EXIT),
    ('calibrate-cross-module-name', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            report['status'] = record(report)\n" + CT_EXIT),
    ('calibrate-exempt-callee-write', SOURCE,
     "            report['status'] = overall\n",
     "            report['status'] = _retirement_census_verdict(report)\n"),
    ('calibrate-quit-exit', CONTROLS_SOURCE,
     CT_EXIT,
     "        if report.get('special'):\n"
     "            quit(0)\n" + CT_EXIT),
    ('calibrate-partial-raise-point', CONTROLS_SOURCE,
     "    save(output / 'results.json', report)\n"
     "    return report\n",
     "    save(output / 'results.json', report)\n"
     "    if report.get('special'):\n"
     "        raise ValueError('x')\n"),
    ('calibrate-param-augassign-opaque', CONTROLS_SOURCE,
     "def run_all(output, source=None):",
     "def publish(report, payload):\n"
     "    report |= payload\n"
     "    return report\n"
     "def run_all(output, source=None):"),
    # #892 fix-r15 (GLM boundary r14 P1-1): the ``|=`` enumeration dropped
    # *module*/*rel*, so the payload call fell back to the name-only registry
    # and a local ``run_all`` in another file was trusted by name.
    ('calibrate-augassign-cross-file-call', GUARD_SOURCE,
     "def _failure(error, policy, common):",
     "def run_all(rows):\n"
     "    return rows[0]\n\n"
     "def publish(report, rows):\n"
     "    report |= run_all(rows)\n"
     "    return report\n\n"
     "def _failure(error, policy, common):"),
    # #892 fix-r15 (MiniMax sibling r14 P2-1): the raise family enumerated a
    # status payload (``raise-exit-status``) while the exit-call family
    # enumerated nothing, so this point never reached the registration check.
    ('calibrate-exit-call-unregistered', GUARD_SOURCE,
     "def _failure(error, policy, common):",
     "def publish(report):\n"
     "    print(report)\n"
     "    sys.exit('BLOCKED')\n\n"
     "def _failure(error, policy, common):"),
    # #892 fix-r15 (MiniMax simplify r14 P2-2): the enumeration only knew the
    # plain subscript target, so the tuple spelling wrote the same slot unseen.
    ('calibrate-tuple-status-target', GUARD_SOURCE,
     "def _failure(error, policy, common):",
     "def publish(rows):\n"
     "    report = {'cases': rows}\n"
     "    report['status'], code = 'PASS', 0\n"
     "    return report\n\n"
     "def _failure(error, policy, common):"),
    # #892 fix-r16 (GLM boundary r15 P1-1): a registered point writes its status
    # through an *attribute* callee whose bare name is registered for this very
    # file, so the name-only lookup inherited the registration and the write was
    # never judged.
    ('calibrate-attr-collision', SOURCE,
     "                    report['status'] = overall\n",
     "                    report['status'] = helpers.cleanup_status(audit)\n"),
    # #892 fix-r16 (MiniMax sibling r15 P1-1): the raise family only enumerated
    # raises carrying a status word, so the most optimistic spellings -- a bare
    # ``raise SystemExit`` and ``raise SystemExit(0)`` -- stayed invisible.
    ('calibrate-bare-raise-exit', GUARD_SOURCE,
     "def _failure(error, policy, common):",
     "def publish(report):\n"
     "    print(report)\n"
     "    raise SystemExit(0)\n\n"
     "def _failure(error, policy, common):"),
    # #892 fix-r17 (GLM boundary r16 P0-1): the exit family matched literal
    # names only, so an import alias walked straight past every rule.
    ('calibrate-alias-sysexit', GUARD_SOURCE,
     "def _failure(error, policy, common):",
     "import sys as s\n\n"
     "def publish(report):\n"
     "    print(report)\n"
     "    s.exit(0)\n\n"
     "def _failure(error, policy, common):"),
    ('calibrate-alias-system-exit', GUARD_SOURCE,
     "def _failure(error, policy, common):",
     "from builtins import SystemExit as SE\n\n"
     "def publish(report):\n"
     "    print(report)\n"
     "    raise SE(0)\n\n"
     "def _failure(error, policy, common):"),
    # #892 fix-r17 (GLM boundary r16 P1-1): the write family knew
    # ``update`` / ``setdefault`` only; ``setattr`` and ``__setitem__`` write the
    # same slot and were read by nothing.
    ('calibrate-setattr-status', SOURCE,
     "                    report['status'] = overall\n",
     "                    setattr(report, 'status', 'PASS')\n"),
    ('calibrate-dunder-setitem-status', SOURCE,
     "                    report['status'] = overall\n",
     "                    report.__setitem__('status', 'PASS')\n"),
    # #892 fix-r18 (GLM boundary r17 P0-1): the alias sources a real tree uses.
    # ``_module_exit_aliases`` walked direct module children and
    # ``_name_bindings`` never collected ``import`` statements, so a
    # function-local or ``try:``-guarded alias was invisible.
    ('calibrate-local-import-alias', GUARD_SOURCE,
     "def _failure(error, policy, common):",
     "def publish(report):\n"
     "    import sys as s\n"
     "    s.exit(0)\n\n"
     "def _failure(error, policy, common):"),
    ('calibrate-module-alias-binding', GUARD_SOURCE,
     "def _failure(error, policy, common):",
     "bail = sys.exit\n\n"
     "def publish(report):\n"
     "    bail(0)\n\n"
     "def _failure(error, policy, common):"),
    # #892 fix-r18 (MiniMax sibling r17 P1-1): the attribute spelling of a
    # status write had no branch in either the enumeration or the slot layer.
    ('calibrate-attr-status', SOURCE,
     "                    report['status'] = overall\n",
     "                    report.status = 'PASS'\n"),
    ('calibrate-dunder-setattr-status', SOURCE,
     "                    report['status'] = overall\n",
     "                    report.__setattr__('status', 'PASS')\n"),
    # #892 fix-r18 (GLM sibling r17 P2-2): the unbound three-argument
    # spellings, which ``_call_written_values`` read as ordinary calls.
    ('calibrate-unbound-setitem-status', SOURCE,
     "                    report['status'] = overall\n",
     "                    dict.__setitem__(report, 'status', 'PASS')\n"),
    ('calibrate-builtins-setattr-status', SOURCE,
     "                    report['status'] = overall\n",
     "                    builtins.setattr(report, 'status', 'PASS')\n"),
    ('calibrate-operator-setitem-status', SOURCE,
     "                    report['status'] = overall\n",
     "                    operator.setitem(report, 'status', 'PASS')\n"),
    ('calibrate-status-getattr-alias', SOURCE,
     "                    report['status'] = overall\n",
     "                    _ga = getattr\n"
     "                    _ga(report, '__setitem__')('status', 'PASS')\n"),
    # #892 fix-r19 (GLM boundary r18 P1-1): the module-level assignment pass
    # walked direct children only, so an alias bound inside ``try:`` -- the
    # shape ``_guard.py`` itself uses -- never reached ``_module_exit_aliases``.
    ('calibrate-try-assign-alias', GUARD_SOURCE,
     "def _failure(error, policy, common):",
     "try:\n"
     "    bail = sys.exit\n"
     "except ImportError:\n"
     "    bail = None\n\n"
     "def publish(report):\n"
     "    bail(0)\n\n"
     "def _failure(error, policy, common):"),
    # #892 fix-r19 (GLM boundary r18 P0-1): the return arm had no branch for an
    # exit code handed back as an attribute or read out of an untracked
    # container, so the value was judged by no rule at all.
    ('calibrate-attr-exit-code', GUARD_SOURCE,
     "    if policy == 'traceback':",
     "    if policy == 'attribute':\n"
     "        return common.aggregate_status('BLOCKED'), helpers.EXIT_OK\n"
     "    if policy == 'traceback':"),
    ('calibrate-subscript-exit-code', GUARD_SOURCE,
     "    if policy == 'traceback':",
     "    if policy == 'subscript':\n"
     "        return common.aggregate_status('BLOCKED'), CODES[policy]\n"
     "    if policy == 'traceback':"),
    ('calibrate-attr-code-relay', GUARD_SOURCE,
     "    if policy == 'traceback':",
     "    if policy == 'relay':\n"
     "        code = helpers.EXIT_OK\n"
     "        return common.aggregate_status('BLOCKED'), code\n"
     "    if policy == 'traceback':"),
    # #892 fix-r19 (MiniMax boundary r18 P0-1): ``else`` may not cancel a
    # handler that completes on its own, and a ``finally`` that always returns
    # means the statement never falls through.
    ('calibrate-try-else-completes', GUARD_SOURCE,
     "def _failure(error, policy, common):",
     "def _code(policy, common):\n"
     "    try:\n"
     "        raise ValueError(policy)\n"
     "    except ValueError:\n"
     "        pass\n"
     "    else:\n"
     "        return 2\n\n"
     "def _failure(error, policy, common):\n"
     "    if policy == 'code':\n"
     "        return _code(policy, common)"),
    # #892 fix-r20 (GLM sibling r19 P1 / MiniMax boundary r19 P0): the r19
    # attribute/subscript rule reached the return arm only; the exit-call
    # argument is judged by ``_exit_provenance``, which had no such branch.
    ('calibrate-exit-call-attribute', GUARD_SOURCE,
     "    if policy == 'traceback':",
     "    if policy == 'exit-attribute':\n"
     "        sys.exit(helpers.EXIT_OK)\n"
     "    if policy == 'traceback':"),
    ('calibrate-exit-call-subscript', GUARD_SOURCE,
     "    if policy == 'traceback':",
     "    if policy == 'exit-subscript':\n"
     "        raise SystemExit(CODES[policy])\n"
     "    if policy == 'traceback':"),
    # #892 fix-r20 (GLM boundary r19 P1-1): the spelling ``common.X`` was
    # trusted without asking whether ``common`` is still the shared module.
    # The mutated ``_guard.py`` is exec'd in a fresh namespace, so the stub
    # carries its own import: without it the file dies at load time and the
    # harness reports INVALID instead of proving the audit caught the rebinding.
    ('calibrate-shadowed-common', GUARD_SOURCE,
     "def _failure(error, policy, common):",
     "from types import SimpleNamespace\n"
     "common = SimpleNamespace(aggregate_status=lambda *a: ('PASS', 0))\n\n\n"
     "def _failure(error, policy, common):"),
    # #892 fix-r20 (GLM boundary r19 P1-2): a clean status assembled out of
    # constants is the same literal to the interpreter, but not a Constant node.
    ('calibrate-computed-status-write', GUARD_SOURCE,
     "    if policy == 'traceback':",
     "    if policy == 'computed':\n"
     "        report = {'status': 'PA' + 'SS'}\n"
     "        return report, 0\n"
     "    if policy == 'traceback':"),
    # #892 fix-r20 (MiniMax boundary r19 P1): a two-hop local exit alias.
    ('calibrate-local-exit-alias-chain', GUARD_SOURCE,
     "def _failure(error, policy, common):",
     "def publish(report):\n"
     "    bail = sys.exit\n"
     "    other = bail\n"
     "    other(0)\n\n\n"
     "def _failure(error, policy, common):"),
    # #892 fix-r21 (GLM sibling r20 P1-1): the r20 constant folding reached
    # the value side only; a summary keyed ``{'sta' + 'tus': ...}`` was not a
    # summary at all, so the function escaped enumeration entirely.
    ('calibrate-computed-status-key', GUARD_SOURCE,
     "    if policy == 'traceback':",
     "    if policy == 'computed-key':\n"
     "        report = {'sta' + 'tus': 'PA' + 'SS'}\n"
     "        return report, 0\n"
     "    if policy == 'traceback':"),
    ('calibrate-computed-status-key-write', GUARD_SOURCE,
     "    if policy == 'traceback':",
     "    if policy == 'computed-key-write':\n"
     "        report = {}\n"
     "        report['sta' + 'tus'] = 'PA' + 'SS'\n"
     "        return report, 0\n"
     "    if policy == 'traceback':"),
    # #892 fix-r21 (GLM sibling r20 P1-2 / boundary r20 P1-1): the r20 trust
    # withdrawal walked module scope only, so a function-scope rebinding of
    # ``common`` (here: over the parameter itself) kept every stub call
    # counted as the shared map's.
    ('calibrate-shadowed-common-local', GUARD_SOURCE,
     "def _failure(error, policy, common):",
     "def _failure(error, policy, common):\n"
     "    from types import SimpleNamespace\n"
     "    common = SimpleNamespace(aggregate_status=lambda *a: ('PASS', 0))\n"),
    # #892 fix-r21 (GLM boundary r20 P1-1, receiver form): a locally rebound
    # relay receiver -- ``md = <stub>`` then ``md.common.aggregate_status`` --
    # was taken on the spelling alone.  The fixture's own summarize_results
    # is the registered point that uses exactly this shape.  The stub yields a
    # FAIL verdict, not a fake PASS: summarize_results is itself the verdict
    # function the harness requires to exit non-zero for a kill, so a
    # PASS-returning stub would mask its own detection exit; the audit being
    # calibrated is static and flags the rebind regardless of the stub's value.
    ('calibrate-shadowed-receiver-local', CHECK,
     "    executed = [row for row in results if row.get('passed') is not None]\n"
     "    skipped = [row for row in results",
     "    from types import SimpleNamespace\n"
     "    md = SimpleNamespace(common=SimpleNamespace(\n"
     "        aggregate_status=lambda *a: ('FAIL', 1)))\n"
     "    executed = [row for row in results if row.get('passed') is not None]\n"
     "    skipped = [row for row in results"),
    # #892 fix-r21 (module poisoning through a function): ``global common``
    # rebinds the module binding at runtime out of the module walk's sight,
    # so the spelling stayed trusted for every function in the file.
    ('calibrate-shadowed-common-global', GUARD_SOURCE,
     "def _failure(error, policy, common):",
     "def _poison_shared():\n"
     "    global common\n"
     "    from types import SimpleNamespace\n"
     "    common = SimpleNamespace(aggregate_status=lambda *a: ('PASS', 0))\n\n\n"
     "def _failure(error, policy, common):"),
    # #892 fix-r23 (GLM boundary r22 P0-1): the relay receiver lost the shared
    # call only when rebound *inside* the audited function; module and global
    # poisoning kept the relay trusted file-wide.  Three shapes, each judged on
    # the registered point (summarize_results) the fixture itself uses; the stub
    # yields FAIL, not a fake PASS, for the reason given at
    # calibrate-shadowed-receiver-local.
    ('calibrate-shadowed-receiver-global', CHECK,
     "    global md\n    md = load_session_domain()\n",
     "    global md\n"
     "    from types import SimpleNamespace\n"
     "    md = SimpleNamespace(common=SimpleNamespace(\n"
     "        aggregate_status=lambda *a: ('FAIL', 1)))\n"),
    ('calibrate-shadowed-receiver-poison-fn', CHECK,
     "def load_session_domain():",
     "def _poison_md():\n"
     "    global md\n"
     "    from types import SimpleNamespace\n"
     "    md = SimpleNamespace(common=SimpleNamespace(\n"
     "        aggregate_status=lambda *a: ('FAIL', 1)))\n"
     "\n"
     "\n"
     "def load_session_domain():"),
    ('calibrate-shadowed-receiver-same-fn', CHECK,
     "    executed = [row for row in results if row.get('passed') is not None]\n"
     "    skipped = [row for row in results",
     "    global md\n"
     "    from types import SimpleNamespace\n"
     "    md = SimpleNamespace(common=SimpleNamespace(\n"
     "        aggregate_status=lambda *a: ('FAIL', 1)))\n"
     "    executed = [row for row in results if row.get('passed') is not None]\n"
     "    skipped = [row for row in results"),
    # #892 fix-r23 (GLM boundary r22 P1-2): the loader alias redefined as a
    # stub-returning def.  The mutated controls file is statically scanned,
    # not imported by the aggregation-audit run, so the mutation cannot crash
    # the audit it is there to calibrate.
    ('calibrate-redefined-loader', CONTROLS_SOURCE,
     "_load = _guard.load\n",
     "_load = _guard.load\n"
     "\n"
     "\n"
     "def _load(name, path):\n"
     "    from types import SimpleNamespace\n"
     "    return SimpleNamespace(aggregate_status=lambda *a: ('PASS', 0))\n"),
    # #892 fix-r25 (GLM boundary r24 P0-1): the binding graph walked only the
    # statements ``_own_nodes`` descends, so a same-named def inside an
    # ``if``/``try`` at module scope left the registered name on one ``[None]``
    # binding and stayed proven while the conditional def ran.
    ('calibrate-conditional-def', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if True:\n"
     "    def load_session_domain():\n"
     "        from types import SimpleNamespace\n"
     "        return SimpleNamespace(common=SimpleNamespace(\n"
     "            aggregate_status=lambda *a: ('FAIL', 1)))\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r25 (GLM concurrency r24 P0-1 / boundary r24 P1-3): a decorator
    # replaces the function object at def time; the binding graph still saw a
    # single ``None`` binding and kept the name proven.
    ('calibrate-decorated-source', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "def _swap_source(_fn):\n"
     "    from types import SimpleNamespace\n"
     "    return lambda *a, **k: SimpleNamespace(common=SimpleNamespace(\n"
     "        aggregate_status=lambda *x: ('FAIL', 1)))\n"
     "\n"
     "\n"
     "@_swap_source\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r25 (GLM boundary r24 P0-2 / concurrency r24 P1-2): a nested
    # ``def`` shadows the imported proven source for the whole body, but
    # ``_own_nodes`` prunes definition subtrees so ``local_names`` missed it.
    ('calibrate-nested-def-rebind', NEGATIVE,
     "    _install_terminate_handlers()\n    eligible = applicable_mutations(sys.platform)\n    known = ",
     "    _install_terminate_handlers()\n"
     "    def committed_status_contract(*_a, **_k):\n"
     "        from types import SimpleNamespace\n"
     "        return SimpleNamespace(aggregate_status=lambda *a: ('FAIL', 1))\n"
     "    eligible = applicable_mutations(sys.platform)\n    known = "),
    # #892 fix-r25 (GLM boundary r24 P0-2): a walrus target binds in the
    # enclosing function scope and no ``Assign`` statement records it.
    ('calibrate-walrus-rebind', NEGATIVE,
     "    _install_terminate_handlers()\n    eligible = applicable_mutations(sys.platform)\n    known = ",
     "    _install_terminate_handlers()\n"
     "    if (committed_status_contract := None) is None:\n"
     "        pass\n"
     "    eligible = applicable_mutations(sys.platform)\n    known = "),
    # #892 fix-r25 (GLM boundary r24 P1-4 / concurrency r24 P2-4): a
    # ``globals()['<registered name>'] = <stub>`` write moves the module
    # binding with no statement the binding graph can see.
    ('calibrate-dynamic-globals-write', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "def _poison_dynamic():\n"
     "    from types import SimpleNamespace\n"
     "    globals()['load_session_domain'] = lambda *a, **k: SimpleNamespace(\n"
     "        common=SimpleNamespace(aggregate_status=lambda *x: ('FAIL', 1)))\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r25 (GLM concurrency r24 P1-3): pre-seeding ``sys.modules``
    # satisfies the ``sys.modules.get('_guard')`` arm with a stub while the
    # bootstrap looks untouched.  Statically scanned only, like
    # calibrate-redefined-loader above.
    ('calibrate-sys-modules-poison', CONTROLS_SOURCE,
     "_guard = sys.modules.get('_guard')\n",
     "from types import SimpleNamespace as _SN_poison\n"
     "sys.modules['_guard'] = _SN_poison(load=lambda *a, **k: None)\n"
     "_guard = sys.modules.get('_guard')\n"),
    # #892 fix-r25 (GLM sibling r24 P1-1): ``global common; common =
    # _load(...)`` was accepted on the module loader spelling even when the
    # same function locally shadows ``_load`` with a stub factory.
    ('calibrate-shadowed-global-loader', CONTROLS_SOURCE,
     "_load = _guard.load\n",
     "_load = _guard.load\n"
     "\n"
     "\n"
     "def _poison_global():\n"
     "    global common\n"
     "    from types import SimpleNamespace\n"
     "    _load = lambda *a, **k: SimpleNamespace(\n"
     "        aggregate_status=lambda *x: ('FAIL', 1))\n"
     "    common = _load('common', _apparatus / 'common.py')\n"),
    # #892 fix-r26 (GLM concurrency r25 P0-1 / boundary r25 P0-2): a
    # module-scope walrus rebinds the proven source with no ``Assign``
    # statement, and every consumer kept reading the ``def`` binding.
    ('calibrate-module-walrus', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if (load_session_domain := (lambda *a, **k: None)):\n"
     "    pass\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r26 (GLM boundary r25 P0-1): ``globals()['md'] = <stub>`` inside
    # the function that owns the relay replaces the module receiver with no
    # binding statement anywhere the module walk reads.
    ('calibrate-dynamic-receiver-write', CHECK,
     "    global md\n    md = load_session_domain()",
     "    global md\n"
     "    globals()['md'] = SimpleNamespace(\n"
     "        common=SimpleNamespace(aggregate_status=lambda *a: ('PASS', 0)))\n"
     "    md = load_session_domain()"),
    # #892 fix-r26 (GLM concurrency r25 P1-6): the shared attribute of the relay
    # replaced key by key, through a literal dict the key spelling comes from.
    ('calibrate-setattr-shared-write', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    for _k, _v in {'common': None}.items():\n"
     "        setattr(md, _k, _v)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r26 (GLM boundary r25 P1-3): ``exec`` whose source is not a
    # literal at all still writes this module's namespace.
    ('calibrate-exec-nonliteral', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "_code = 'load_session_domain = None'\n"
     "exec(_code)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r26 (GLM sibling r25 #2 / boundary r25 P1-5): the registry keys
    # on the importing file, so a same-named redefinition in the *defining*
    # file kept every importer's trust.
    ('calibrate-negative-process-redefine', NEGATIVE_PROCESS,
     "def committed_status_contract(",
     "def committed_status_contract(*_a, **_k):\n"
     "    return None\n"
     "\n"
     "\n"
     "def committed_status_contract("),
    # #892 fix-r26 (GLM sibling r25 #5 / boundary r25 P1-7): a decorator
    # replaces the sanctioned loader object while its ``[None]`` binding stays.
    ('calibrate-decorated-guard-load', GUARD_SOURCE,
     "def load(name, path):",
     "def _swap(fn):\n"
     "    return fn\n"
     "\n"
     "\n"
     "@_swap\n"
     "def load(name, path):"),
    # #892 fix-r27 (DeepSeek sibling r26 P1-4): ``md.common = <stub>`` is the
    # same withdrawal as ``setattr(md, 'common', ...)`` -- only the call
    # spelling was judged.
    ('calibrate-direct-attribute-write', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    md.common = SimpleNamespace(\n"
     "        aggregate_status=lambda *a: ('PASS', 0))\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r27 (GLM boundary r26 P0-2): the literal dict a ``setattr`` key
    # loop walks is mutated after its binding, so the key set is unbounded and
    # cannot be read as the binding-site literal.
    ('calibrate-dict-mutation-keys', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "_KEYS = {'common': None}\n"
     "_KEYS['main'] = None\n"
     "if False:\n"
     "    for _k in _KEYS:\n"
     "        setattr(md, _k, None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r27 (GLM boundary r26 P0-3): a key that is not a literal cannot
    # be resolved to the one entry it pre-seeds, so the ``'*'`` sentinel must be
    # a real verdict instead of dead data.
    ('calibrate-sys-modules-wildcard', CONTROLS_SOURCE,
     "_guard = sys.modules.get('_guard')\n",
     "_key = '_guard'\n"
     "sys.modules[_key] = object()\n"
     "_guard = sys.modules.get('_guard')\n"),
    # #892 fix-r27 (DeepSeek concurrency r26 P1-8): ``exec`` of a literal
    # source that rebinds a proven source leaves the single-definition rule
    # reading the ``def`` it never replaced.
    ('calibrate-single-definition-dynamic', NEGATIVE_PROCESS,
     "def committed_status_contract(",
     "exec('committed_status_contract = None')\n"
     "\n"
     "\n"
     "def committed_status_contract("),
    # #892 fix-r27 (GLM concurrency r26 P1-1): a literal ``exec`` that rebinds
    # the shared aggregator name is the same withdrawal as ``globals()[...]``.
    ('calibrate-dynamic-shared-name', CONTROLS_SOURCE,
     "_load = _guard.load\n",
     "_load = _guard.load\n"
     "exec('common = None')\n"),
    # #892 fix-r27 (DeepSeek boundary r26 P1-2): ``from builtins import
    # setattr as _sa`` is the same callable as the bare name.
    ('calibrate-builtin-alias-setattr', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "from builtins import setattr as _builtin_setattr\n"
     "if False:\n"
     "    _builtin_setattr(md, 'common', None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r27 (GLM simplify r26 P1-2): ``vars(md)['common'] = <stub>``
    # writes the receiver through its ``__dict__``.
    ('calibrate-vars-dict-receiver', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    vars(md)['common'] = None\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r27 (DeepSeek concurrency r26 P1-3): the ``for`` target that
    # spells the ``setattr`` key is reassigned inside the loop body, so the key
    # is no longer the sequence's spelling.
    ('calibrate-loop-key-rebind', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    for _k in ('common',):\n"
     "        _k = 'main'\n"
     "        setattr(md, _k, None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r27 (GLM concurrency r26 P1-2): a walrus in a definition *header*
    # runs in the enclosing scope even though the body does not.
    ('calibrate-definition-header-walrus', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "def _header(_x=(md := SimpleNamespace(\n"
     "        common=SimpleNamespace(aggregate_status=lambda *a: ('PASS', 0))))):\n"
     "    return _x\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r27 (GLM concurrency r26 P1-1): a key pre-seeded from another
    # pre-seeded key's *read* laundered itself through the one-pass poisoning
    # scan.
    ('calibrate-sys-modules-chain-poison', CONTROLS_SOURCE,
     "_guard = sys.modules.get('_guard')\n",
     "sys.modules['_stub_alias'] = object()\n"
     "sys.modules['_guard'] = sys.modules.get('_stub_alias')\n"
     "_guard = sys.modules.get('_guard')\n"),
    # #892 fix-r27 (GLM boundary r26 P2-1): a walrus inside an audited function
    # rebinds the receiver for the whole body with no ``Assign`` statement.
    ('calibrate-walrus-receiver-local', CHECK,
     "    executed = [row for row in results if row.get('passed') is not None]",
     "    if (md := SimpleNamespace(\n"
     "            common=SimpleNamespace(aggregate_status=lambda *a: ('PASS', 0)))):\n"
     "        pass\n"
     "    executed = [row for row in results if row.get('passed') is not None]"),
    # #892 fix-r28 (r27 自审实测): ``md.__dict__.update(common=<stub>)`` is the
    # subscript write one call away; r27 read ``update``/``setdefault`` only for
    # the module namespace (``globals().update(...)``).
    ('calibrate-dict-namespace-update', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    md.__dict__.update(common=None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r28: ``md.__dict__ |= {...}`` replaces the mapping in place.
    ('calibrate-dict-namespace-augassign', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    md.__dict__ |= {'common': None}\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r28: ``md.__setattr__('common', <stub>)`` is the bound method
    # spelling of the builtin r27 resolved.
    ('calibrate-dunder-setattr-receiver', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    md.__setattr__('common', None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r28: ``object.__setattr__(md, 'common', <stub>)`` is the unbound
    # spelling that bypasses ``builtins.setattr`` altogether.
    ('calibrate-object-setattr-unbound', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    object.__setattr__(md, 'common', None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r28: a name bound to the receiver's ``__dict__`` writes the same
    # mapping one alias away from ``vars(md)[...]``.
    ('calibrate-dict-alias-write', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _receiver_dict = md.__dict__\n"
     "    _receiver_dict['common'] = None\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r28: ``__import__('sys')`` names the same module as
    # ``import sys``, so the module-object setattr path must resolve it.
    ('calibrate-import-call-modules', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    __import__('sys').modules[__name__].__setattr__(\n"
     "        'load_session_domain', lambda *a, **k: None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r29 (r28 自审实测): the unbound spelling names its target first
    # -- ``dict.update(md.__dict__, common=<stub>)``.
    ('calibrate-dict-update-unbound', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    dict.update(md.__dict__, common=None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r29: ``dict.setdefault(md.__dict__, 'common', <stub>)``.
    ('calibrate-dict-setdefault-unbound', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    dict.setdefault(md.__dict__, 'common', None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r29: ``dict.pop(md.__dict__, 'common', <stub>)``.
    ('calibrate-dict-pop-unbound', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    dict.pop(md.__dict__, 'common', None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r29: ``dict.__setitem__(md.__dict__, 'common', <stub>)`` -- this
    # path was already red in r28; the mutant pins it.
    ('calibrate-dict-setitem-unbound', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    dict.__setitem__(md.__dict__, 'common', None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r29: a helper module's ``setattr`` wrapper is the builtin's.
    ('calibrate-helper-setattr', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    attr.setattr(md, 'common', None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r30: the managed-write spellings r29's resolver still missed --
    # ``operator.setitem`` / ``getattr`` / ``functools.partial`` targets, a
    # tuple assignment target, and the namespace writes spelled through an
    # alias, a literal ``eval``/``exec`` namespace or an unbound mapping call.
    ('calibrate-operator-setitem', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    operator.setitem(md.__dict__, 'common', None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-getattr-dunder-write', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    getattr(md, '__setattr__')('common', None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-getattr-builtin-write', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    import builtins as _bi\n"
     "    getattr(_bi, 'setattr')(md, 'common', None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-partial-write', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    import functools as _ft\n"
     "    _ft.partial(setattr, md)('common', None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-partial-handle', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    import functools as _ft\n"
     "    _p = _ft.partial(setattr, md)\n"
     "    _p('common', None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-tuple-attribute-write', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    md.common, _x = None, None\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-eval-literal-write', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    eval(\"setattr(md, 'common', None)\")\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-exec-receiver-dict', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    exec('common = None', md.__dict__)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-locals-write', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    locals()['common'] = None\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-namespace-unbound-update', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    dict.update(globals(), common=None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-builtins-type-alias', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    from builtins import dict as _bd\n"
     "    _bd.update(md.__dict__, common=None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-dict-alias-augassign', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _d = md.__dict__\n"
     "    _d |= {'common': None}\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-walrus-receiver-write', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    (m := md)\n"
     "    setattr(m, 'common', None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-alias-chain-reverse', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _e = _d\n"
     "    _d = _c\n"
     "    _c = _b\n"
     "    _b = _a\n"
     "    _a = md.__dict__\n"
     "    _e['common'] = None\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r30: the ``sys.modules`` pre-seed through a loop pair, a loop
    # key rebound in the body, and the unbound ``dict.update`` spelling.
    ('calibrate-sm-pair-value', CONTROLS_SOURCE,
     "_guard = sys.modules.get('_guard')\n",
     "_saved = sys.modules.get('_guard')\n"
     "for _n, _p in (('_guard', _saved),):\n"
     "    sys.modules[_n] = object()\n"
     "_guard = sys.modules.get('_guard')\n"),
    ('calibrate-sm-loop-rebind', CONTROLS_SOURCE,
     "_guard = sys.modules.get('_guard')\n",
     "_saved = sys.modules.get('_guard')\n"
     "for _n, _p in (('common', _saved),):\n"
     "    _n = '_guard'\n"
     "    sys.modules[_n] = object()\n"
     "_guard = sys.modules.get('_guard')\n"),
    ('calibrate-sm-unbound-update', CONTROLS_SOURCE,
     "_guard = sys.modules.get('_guard')\n",
     "dict.update(sys.modules, {'_guard': object()})\n"
     "_guard = sys.modules.get('_guard')\n"),
    ('calibrate-getattr-dict-subscript', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    getattr(md, '__dict__')['common'] = None\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-builtins-subscript-setattr', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    __builtins__['setattr'](md, 'common', None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-getattr-import-alias', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    from builtins import getattr as _pdrs_g\n"
     "    _pdrs_g(md, '__dict__')['common'] = None\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-builtins-module-alias', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    import builtins as _pdrs_b\n"
     "    _pdrs_b.getattr(md, '__dict__')['common'] = None\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-builtins-getattr-subscript', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    __builtins__['getattr'](md, '__dict__')['common'] = None\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-builtins-subscript-vars', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    __builtins__['vars'](md)['common'] = None\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    # #892 fix-r32 (codex boundary r31 P0-2/P0-3/P0-5..P0-10, GLM
    # sibling r31 P1-3/P3-5, concurrency r31 P3-1): the write handle,
    # the star argument and the binding forms the r31 rules could not name.
    ('calibrate-write-handle-name', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _fh = md.__setattr__\n"
     "    _fh('common', None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-write-handle-getattr', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _fh = getattr(md, '__setattr__')\n"
     "    _fh('common', None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-write-handle-attrgetter', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    import operator as _pdrs_o\n"
     "    _pdrs_o.attrgetter('__setattr__')(md)('common', None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-star-setattr', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    setattr(*(md, 'common', None))\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-star-forward', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    def _pdrs_f(*a, **kw):\n"
     "        setattr(*a, **kw)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-unpack-receiver', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _pdrs_m, _pdrs_other = md, None\n"
     "    _pdrs_m.common = None\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-for-receiver', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    for _pdrs_m in (md,):\n"
     "        _pdrs_m.common = None\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-default-receiver', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    def _pdrs_f(_pdrs_m=md):\n"
     "        _pdrs_m.common = None\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-with-receiver', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    with nullcontext(md) as _pdrs_m:\n"
     "        _pdrs_m.common = None\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-with-key-rebind', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    for _pdrs_k in {'safe': 1}:\n"
     "        with nullcontext('common') as _pdrs_k:\n"
     "            setattr(md, _pdrs_k, None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-globals-tuple-target', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    globals()['common'], _pdrs_x = None, None\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-sm-callable-setitem', CONTROLS_SOURCE,
     "_guard = sys.modules.get('_guard')\n",
     "from operator import setitem as _pdrs_put\n"
     "_pdrs_put(sys.modules, '_guard', object())\n"
     "_guard = sys.modules.get('_guard')\n"),
    ('calibrate-sm-mapping-augassign', CONTROLS_SOURCE,
     "_guard = sys.modules.get('_guard')\n",
     "sys.modules |= {'_guard': object()}\n"
     "_guard = sys.modules.get('_guard')\n"),
    ('calibrate-builtins-alias-subscript', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    import builtins as _pdrs_b\n"
     "    _pdrs_bs = _pdrs_b\n"
     "    _pdrs_bs['setattr'](md, 'common', None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-getattr-dynamic-call', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _pdrs_n = '__setattr__'\n"
     "    getattr(md, _pdrs_n)('common', None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-globals-alias-augassign', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _pdrs_g = globals()\n"
     "    _pdrs_g |= {'common': None}\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),    # #892 fix-r33 (codex boundary r32 P0-1..P0-5/P1-1/P1-2, GLM
    # boundary r32 P0-1/P1-2/P1-3/P2-5, concurrency r32 P1-1): the
    # bindings the r32 rules still could not name, the receivers they
    # withdrew for unrelated objects, and the alias chains they cut at
    # three hops.
    ('calibrate-global-walrus-rebind', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n    def _pdrs_walrus():\n        global md\n        (md := object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-unpack-star-target', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n    _m, *_r = (md, None)\n    _m.common = None\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-unpack-star-value', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n    _m, _x = *(md,), None\n    _m.common = None\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-unpack-iter-for', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n    for _m in iter((md,)):\n        _m.common = None\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-with-handle-name', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n    _h = nullcontext(md)\n    with _h as _m:\n        _m.common = None\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-getattr-handle-name', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n    _n = '__setattr__'\n    _z = getattr(md, _n)\n    _z('common', None)\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-getattr-handle-dict', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n    _n = '__dict__'\n    _d = getattr(md, _n)\n    _d['common'] = None\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-nonlocal-common-rebind', SOURCE,
     "                    report['status'] = overall\n",
     "                    if False:\n                        def _pdrs_outer(common):\n                            def _pdrs_inner():\n                                nonlocal common\n                                common = object()\n                            _pdrs_inner()\n                            return common.aggregate_status('PASS')\n                        _, _c = _pdrs_outer(common)\n"
     "                    report['status'] = overall\n"),
    ('calibrate-builtin-last-binding', SOURCE,
     "                    report['status'] = overall\n",
     "                    if False:\n                        dict = builtins.dict\n                        dict = report\n                        dict.update({'status': 'PASS'})\n"
     "                    report['status'] = overall\n"),
    ('calibrate-partial-prebound', SOURCE,
     "                    report['status'] = overall\n",
     "                    if False:\n                        import functools, operator\n                        functools.partial(operator.setitem, report, 'status')('PASS')\n"
     "                    report['status'] = overall\n"),
    ('calibrate-partial-name-status', SOURCE,
     "                    report['status'] = overall\n",
     "                    if False:\n                        import functools\n                        p = functools.partial(setattr, report)\n                        p('status', 'PASS')\n"
     "                    report['status'] = overall\n"),
    ('calibrate-builtins-chain-four', SOURCE,
     "                    report['status'] = overall\n",
     "                    if False:\n                        bi1 = __builtins__\n                        bi2 = bi1\n                        bi3 = bi2\n                        bi4 = bi3\n                        bi4['setattr'](report, 'status', 'PASS')\n"
     "                    report['status'] = overall\n"),

    # #892 fix-r34 (codex boundary r33 P0-1..P0-6, P1-7..P1-9): the
    # order-sensitive bindings, the closure defaults, the star forms and the
    # prebound partials the r33 resolver still lost.
    ('calibrate-closure-default', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    def _outer():\n"
     "        _x = md\n"
     "        def _inner(_x=_x):\n"
     "            _x.common = object()\n"
     "        _inner()\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-global-handoff', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    def _f():\n"
     "        global _alias\n"
     "        _y = md\n"
     "        _alias = _y\n"
     "    _f()\n"
     "    _alias.common = object()\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-star-from-name', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _xs = (md,)\n"
     "    _m, _x = (*_xs, None)\n"
     "    _m.common = object()\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-star-target-rest', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _a, *_rest = (None, md)\n"
     "    _rest[0].common = object()\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-handle-annassign', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _h: object = getattr(md, _n)\n"
     "    _h('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-handle-argument', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    def _invoke(_h):\n"
     "        _h('common', object())\n"
     "    _invoke(getattr(md, _n))\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-ordered-binding', SOURCE,
     "                    report['status'] = overall\n",
     "                    if False:\n"
     "                        import builtins\n"
     "                        dict = report\n"
     "                        dict.update({'status': 'PASS'})\n"
     "                        dict = builtins.dict\n"
     "                    report['status'] = overall\n"),
    ('calibrate-partial-update-prebound', SOURCE,
     "                    report['status'] = overall\n",
     "                    if False:\n"
     "                        import functools\n"
     "                        functools.partial(dict.update, report, {'status': 'PASS'})()\n"
     "                    report['status'] = overall\n"),
    ('calibrate-partial-setdefault-prebound', SOURCE,
     "                    report['status'] = overall\n",
     "                    if False:\n"
     "                        import functools\n"
     "                        functools.partial(dict.setdefault, report, 'status')('PASS')\n"
     "                    report['status'] = overall\n"),
    ('calibrate-builtins-chain-fiftyone', SOURCE,
     "                    report['status'] = overall\n",
     "                    if False:\n"
     "                        bi0 = __builtins__\n"
     "                        bi1 = bi0\n"
     "                        bi2 = bi1\n"
     "                        bi3 = bi2\n"
     "                        bi4 = bi3\n"
     "                        bi5 = bi4\n"
     "                        bi6 = bi5\n"
     "                        bi7 = bi6\n"
     "                        bi8 = bi7\n"
     "                        bi9 = bi8\n"
     "                        bi10 = bi9\n"
     "                        bi11 = bi10\n"
     "                        bi12 = bi11\n"
     "                        bi13 = bi12\n"
     "                        bi14 = bi13\n"
     "                        bi15 = bi14\n"
     "                        bi16 = bi15\n"
     "                        bi17 = bi16\n"
     "                        bi18 = bi17\n"
     "                        bi19 = bi18\n"
     "                        bi20 = bi19\n"
     "                        bi21 = bi20\n"
     "                        bi22 = bi21\n"
     "                        bi23 = bi22\n"
     "                        bi24 = bi23\n"
     "                        bi25 = bi24\n"
     "                        bi26 = bi25\n"
     "                        bi27 = bi26\n"
     "                        bi28 = bi27\n"
     "                        bi29 = bi28\n"
     "                        bi30 = bi29\n"
     "                        bi31 = bi30\n"
     "                        bi32 = bi31\n"
     "                        bi33 = bi32\n"
     "                        bi34 = bi33\n"
     "                        bi35 = bi34\n"
     "                        bi36 = bi35\n"
     "                        bi37 = bi36\n"
     "                        bi38 = bi37\n"
     "                        bi39 = bi38\n"
     "                        bi40 = bi39\n"
     "                        bi41 = bi40\n"
     "                        bi42 = bi41\n"
     "                        bi43 = bi42\n"
     "                        bi44 = bi43\n"
     "                        bi45 = bi44\n"
     "                        bi46 = bi45\n"
     "                        bi47 = bi46\n"
     "                        bi48 = bi47\n"
     "                        bi49 = bi48\n"
     "                        bi50 = bi49\n"
     "                        bi50['setattr'](report, 'status', 'PASS')\n"
     "                    report['status'] = overall\n"),

    # #1953 fix-r35 (issue A..G): the nine constructs the r34 resolver still
    # read as clean.  Each one is a real write to the shared aggregator with a
    # construct the analyser lost sight of.
    ('calibrate-loop-backedge-alias', SOURCE,
     "                    report['status'] = overall\n",
     "                    if False:\n"
     "                        import builtins\n"
     "                        dict = builtins.dict\n"
     "                        for i in (0, 1):\n"
     "                            if i:\n"
     "                                dict.update({'status': 'PASS'})\n"
     "                            dict = report\n"
     "                    report['status'] = overall\n"),
    ('calibrate-sameline-restore', SOURCE,
     "                    report['status'] = overall\n",
     "                    if False:\n"
     "                        import builtins\n"
     "                        dict = report; dict.update({'status': 'PASS'}); "
     "dict = builtins.dict\n"
     "                    report['status'] = overall\n"),
    ('calibrate-shared-seen-paths', SOURCE,
     "                    report['status'] = overall\n",
     "                    if False:\n"
     "                        a = setattr\n"
     "                        h = a\n"
     "                        h = a\n"
     "                        h(report, 'status', 'PASS')\n"
     "                    report['status'] = overall\n"),
    ('calibrate-decorator-lambda-scope', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    def outer():\n"
     "        x = md\n"
     "\n"
     "        @((lambda y=x: (lambda f: (setattr(y, 'common', object()), f)[1]))())\n"
     "        def inner():\n"
     "            x = object()\n"
     "\n"
     "    outer()\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-nonlocal-walrus-withdraw', GUARD_SOURCE,
     "    if policy == 'round':\n",
     "    if False:\n"
     "        def middle():\n"
     "            def inner():\n"
     "                nonlocal common\n"
     "                import types\n"
     "                (common := types.SimpleNamespace(\n"
     "                    aggregate_status=lambda *a: ('PASS', 0)))\n"
     "            inner()\n"
     "        middle()\n"
     "    if policy == 'round':\n"),
    ('calibrate-handle-list-literal', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _hs = [getattr(md, _n)]\n"
     "    _hs[0]('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-handle-dict-literal', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _hs = {'x': getattr(md, _n)}\n"
     "    _hs['x']('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-handle-yield', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    def _handles():\n"
     "        yield getattr(md, _n)\n"
     "    next(_handles())('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-getattribute-handover', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _pdrs_attr_name = '__getattribute__'\n"
     "    getattr(md, _pdrs_attr_name)('__dict__')['common'] = object()\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-handle-dictcomp-escape', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _hs = {k: getattr(md, k) for k in (_n,)}\n"
     "    _hs['__setattr__']('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-handle-listcomp-escape', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _hs = [getattr(md, k) for k in (_n,)]\n"
     "    _hs[0]('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-handle-aliased-escape', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _hs = [getattr(md, k) for k in (_n,)]\n"
     "    _f = _hs[0]\n"
     "    _f('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-getattribute-write-attribute', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    getattr(md, '__getattribute__')('__setattr__')"
     "('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-getattr-const-subscript-write', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _keys = ('__setattr__',)\n"
     "    getattr(md, _keys[0])('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-match-capture-helper', SOURCE,
     "                    report['status'] = overall\n",
     "                    if False:\n"
     "                        import operator\n"
     "                        match operator.setitem:\n"
     "                            case h:\n"
     "                                h(report, 'status', 'PASS')\n"
     "                    report['status'] = overall\n"),
    ('calibrate-comprehension-write-helper', SOURCE,
     "                    report['status'] = overall\n",
     "                    if False:\n"
     "                        import operator\n"
     "                        [h(report, 'status', 'PASS')\n"
     "                         for h in [operator.setitem]]\n"
     "                    report['status'] = overall\n"),
    ('calibrate-mixed-alias-binding', SOURCE,
     "                    report['status'] = overall\n",
     "                    if False:\n"
     "                        import operator\n"
     "                        h = lambda *a: None\n"
     "                        h = operator.setitem\n"
     "                        h(report, 'status', 'PASS')\n"
     "                    report['status'] = overall\n"),
    ('calibrate-handle-container-alias', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _hs = [getattr(md, k) for k in (_n,)]\n"
     "    _other = _hs\n"
     "    _other[0]('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-handle-mapping-write', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _hs = [getattr(md, k) for k in (_n,)]\n"
     "    _hs[0]['common'] = object()\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-list-const-subscript-write', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _keys = ['__doc__']\n"
     "    _keys[0] = '__setattr__'\n"
     "    getattr(md, _keys[0])('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-comprehension-filter-write', SOURCE,
     "                    report['status'] = overall\n",
     "                    if False:\n"
     "                        import operator\n"
     "                        [_r for _r in (1,)\n"
     "                         if operator.setitem(report, 'status', 'PASS')]\n"
     "                    report['status'] = overall\n"),
    ('calibrate-match-capture-receiver', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    match md:\n"
     "        case h:\n"
     "            h.common = object()\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-match-capture-setattr-receiver', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    match md:\n"
     "        case h:\n"
     "            setattr(h, 'common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-getattribute-attr-spelling', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    md.__getattribute__('__dict__')['common'] = object()\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-getattribute-name-spelling', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _key = '__dict__'\n"
     "    md.__getattribute__(_key)['common'] = object()\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-for-wrapped-container', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _hs = [getattr(md, k) for k in (_n,)]\n"
     "    for _h in list(_hs):\n"
     "        _h('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-for-target-after-loop', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _hs = [getattr(md, k) for k in (_n,)]\n"
     "    for _h in _hs:\n"
     "        pass\n"
     "    _h('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-for-slice', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _hs = [getattr(md, k) for k in (_n,)]\n"
     "    for _h in _hs[:]:\n"
     "        _h('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-for-copy', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _hs = [getattr(md, k) for k in (_n,)]\n"
     "    for _h in _hs.copy():\n"
     "        _h('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-comp-result-receiver', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _g = [_h for _h in (md,)]\n"
     "    _g[0].common = object()\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-for-target-rebind-if', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _hs = [getattr(md, k) for k in (_n,)]\n"
     "    for _h in _hs:\n"
     "        pass\n"
     "    if False:\n"
     "        _h = None\n"
     "    _h('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-for-target-after-match', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _hs = [getattr(md, k) for k in (_n,)]\n"
     "    for _h in _hs:\n"
     "        pass\n"
     "    match 0:\n"
     "        case 0:\n"
     "            _h('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-comp-dict-result', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _g = {0: _h for _h in (md,)}\n"
     "    _g[0].common = object()\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-comp-name-iter', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _items = (md,)\n"
     "    _g = [_h for _h in _items]\n"
     "    _g[0].common = object()\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-for-target-return', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    def _leak():\n"
     "        _n = '__setattr__'\n"
     "        _hs = [getattr(md, k) for k in (_n,)]\n"
     "        for _h in _hs:\n"
     "            pass\n"
     "        return _h\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-for-target-alias', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _hs = [getattr(md, k) for k in (_n,)]\n"
     "    for _h in _hs:\n"
     "        pass\n"
     "    _f = _h\n"
     "    _f('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-starred-const-subscript-write', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _more = ('__setattr__',)\n"
     "    _keys = ('__doc__', *_more)\n"
     "    getattr(md, _keys[-2])('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-comp-handle-relay', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _hs = [getattr(md, k) for k in (_n,)]\n"
     "    _g = [_h for _h in _hs]\n"
     "    _g[0]('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-for-target-finally-rebind', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _hs = [getattr(md, k) for k in (_n,)]\n"
     "    try:\n"
     "        for _h in _hs:\n"
     "            pass\n"
     "        raise RuntimeError()\n"
     "        _h = None\n"
     "    finally:\n"
     "        _h('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-for-target-closure', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _hs = [getattr(md, k) for k in (_n,)]\n"
     "    for _h in _hs:\n"
     "        pass\n"
     "    def _inner():\n"
     "        _h('common', object())\n"
     "    _inner()\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-for-target-shadowed-fresh', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _hs = [getattr(md, k) for k in (_n,)]\n"
     "    object = lambda: _hs[0]\n"
     "    for _h in _hs:\n"
     "        pass\n"
     "    _h = object()\n"
     "    _h('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-for-target-with-alias', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _hs = [getattr(md, k) for k in (_n,)]\n"
     "    for _h in _hs:\n"
     "        pass\n"
     "    import contextlib as _cl\n"
     "    with _cl.nullcontext(_h) as _f:\n"
     "        _f('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-for-target-except-alias', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _hs = [getattr(md, k) for k in (_n,)]\n"
     "    for _h in _hs:\n"
     "        pass\n"
     "    try:\n"
     "        raise Exception(_h)\n"
     "    except Exception as _e:\n"
     "        _f = _e.args[0]\n"
     "        _f('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-for-target-name-iter', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _hs = [getattr(md, k) for k in (_n,)]\n"
     "    _items = (_hs[0],)\n"
     "    for _h in _items:\n"
     "        pass\n"
     "    _f = _h\n"
     "    _f('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-comp-handle-relay-for', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _hs = [getattr(md, k) for k in (_n,)]\n"
     "    _g = [_h for _h in _hs]\n"
     "    for _f in _g:\n"
     "        _f('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-comp-handle-relay-chain', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     "if False:\n"
     "    _n = '__setattr__'\n"
     "    _hs = [getattr(md, k) for k in (_n,)]\n"
     "    _g = [_h for _h in _hs]\n"
     "    _j = [_f for _f in _g]\n"
     "    _j[0]('common', object())\n"
     "\n"
     "\n"
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)"),
    ('calibrate-for-target-assign-call', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    _x = [_h('common', object())]\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-def-default', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    def _inner(_x=_h('common', object())):\n"
      "        pass\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-readonly-call-leak', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    print(_helper(_h))\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-nested-param', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    def _inner():\n"
      "        def _unused(_h):\n"
      "            pass\n"
      "        _h('common', object())\n"
      "    _inner()\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-compare-call-leak', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    if _sink(_h) == 0:\n"
      "        pass\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-outer-shadow-fresh', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    object = lambda: _hs[0]\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    _h = object()\n"
      "    _h('common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-outer-shadow-print', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    print = _sink\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    _g = lambda: print(_h)\n"
      "    _g()\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-ambiguous-literal', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _items = ()\n"
      "    _items = (md,)\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    for _m in _items:\n"
      "        _m.common = object()\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-comp-element-call', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    _g = [_h('common', object()) for _j in (md,)]\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-call-attribute', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    _h.__call__('common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-call-attribute-alias', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    _f = _h.__call__\n"
      "    _f('common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-false-filter-write', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    _g = [_m for _m in (md,)"
      " if setattr(_m, 'common', object()) if False]\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-class-body-clear', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    class _C:\n"
      "        _h = object()\n"
      "    _h('common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-with-suppress', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    with suppress(ValueError):\n"
      "        raise ValueError()\n"
      "        _h = object()\n"
      "    _h('common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-class-body-shadow', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    class _C:\n"
      "        print = _sink\n"
      "        print(_h)\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-lambda-default', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    _f = lambda _x=_h('common', object()): None\n"
      "    _f()\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-class-body-def', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    class _C:\n"
      "        def _h():\n"
      "            pass\n"
      "    _h('common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-method-self', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    _x = _h.__self__\n"
      "    _x.common = object()\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    # #1953 fix-r1 round 11 (codex r7 five P0).
    ('calibrate-for-target-conditional-literal-rebind', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _items = (md,)\n"
      "    if _flag:\n"
      "        _items = ()\n"
      "    for _x in _items:\n"
      "        _x.common = object()\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-enclosing-binding-after-use', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    def _early():\n"
      "        for _x in _items:\n"
      "            _x.common = object()\n"
      "    _items = (md,)\n"
      "    _early()\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-tuple-partial-alias', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    _f, _x = _h, [_h('common', object())]\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-comp-partial-shadow', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    _f = _h\n"
      "    [_f('common', object()) for _h in (None,)]\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-assert-msg', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    try:\n"
      "        assert False, _h\n"
      "    except AssertionError as _e:\n"
      "        _e.args[0]('common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    # #1953 fix-r1 round 12 (codex r8 P0-1..P0-4): the round-11 class-body and
    # enclosing-scope rules each answered one shape and missed the next.
    ('calibrate-for-target-enclosing-call-between-rebinds', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _items = (md,)\n"
      "    def _early():\n"
      "        for _x in _items:\n"
      "            _x.common = object()\n"
      "    _early()\n"
      "    _items = ()\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-class-body-compound', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    class _C:\n"
      "        if _flag:\n"
      "            _h('common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-class-method-closure', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    class _C:\n"
      "        _h = lambda *a: None\n"
      "        def _method(self):\n"
      "            _h('common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-class-body-shadowed-constructor', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    class _C:\n"
      "        object = lambda: _hs[0]\n"
      "        _h = object()\n"
      "        _h('common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-module-import-alias', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    from builtins import setattr as _mw\n"
      "    def _mw_use():\n"
      "        _mw(md, 'common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-class-body-copy-alias', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    class _C:\n"
      "        _f = _h\n"
      "        _f('common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-nested-class-method', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    class _C:\n"
      "        _h = lambda *a: None\n"
      "        class _D:\n"
      "            def _method(self):\n"
      "                _h('common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    # #1953 fix-r1 round 14 (codex r10 P0-1..P0-4): which binding a use reads
    # is a flow question -- a branch, a loop back edge, a column on the same
    # line, and a class-body block each decide it.
    ('calibrate-for-target-conditional-rebind', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    from builtins import setattr as _cw\n"
      "    if _flag:\n"
      "        from builtins import print as _cw\n"
      "    _cw(md, 'common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-loop-back-edge', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    from builtins import print as _lw\n"
      "    for _x in ():\n"
      "        _lw(md, 'common', object())\n"
      "        from builtins import setattr as _lw\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-same-line-rebind', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    from builtins import setattr as _sw;"
      " _sw(md, 'common', object());"
      " from builtins import print as _sw\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-nested-copy-alias', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    def _g():\n"
      "        if True:\n"
      "            _f = _h\n"
      "        _f('common', object())\n"
      "    _g()\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-class-block-alias', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    _n = '__setattr__'\n"
      "    _hs = [getattr(md, k) for k in (_n,)]\n"
      "    for _h in _hs:\n"
      "        pass\n"
      "    class _C:\n"
      "        if True:\n"
      "            _f = _h\n"
      "        _f('common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    # #1953 fix-r1 round 15 (codex r14 P0-1 / P0-2 / P0-4): the enclosing
    # binding is read at the call, an assignment whose right side names a
    # write helper keeps the spelling one, and the first resolution of a
    # module scope sees its loop back edges.  Each row failed before round 15.
    ('calibrate-for-target-module-call-before-rebind', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    from builtins import setattr as _mc\n"
      "    def _mc_use():\n"
      "        _mc(md, 'common', object())\n"
      "    _mc_use()\n"
      "    from builtins import print as _mc\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-module-self-assignment', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    from builtins import setattr as _ms\n"
      "    _ms = _ms\n"
      "    def _ms_use():\n"
      "        _ms(md, 'common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-module-loop-first-use', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    from builtins import print as _ml\n"
      "    for _ml_x in ():\n"
      "        _ml(md, 'common', object())\n"
      "        from builtins import setattr as _ml\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    # #1953 fix-r1 round 16 (codex r15 P0-1 / P0-2 / P0-3 / P0-4): one row per
    # spelling the round-15 ruler still missed.  A write handle taken into a
    # name and then handed through a duplicated unpack target, through a call
    # site on a loop back edge, through a definition header (defaults run at
    # module level), or through an assignment on a loop back edge.
    ('calibrate-for-target-duplicated-unpack-target', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    def _du_use():\n"
      "        _du = getattr(md, '__setattr__')\n"
      "        _df, _df = None, _du\n"
      "        _df('common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-module-call-site-loop-edge', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    from builtins import print as _cw\n"
      "    def _cw_use():\n"
      "        _cw(md, 'common', object())\n"
      "    for _cw_i in (0, 1):\n"
      "        _cw_use()\n"
      "        from builtins import setattr as _cw\n"
      "    from builtins import print as _cw\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-default-value-call', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    from builtins import setattr as _dv\n"
      "    def _dv_use():\n"
      "        _dv(md, 'common', object())\n"
      "    def _dv_holder(_x=_dv_use()):\n"
      "        pass\n"
      "    from builtins import print as _dv\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-assignment-back-edge', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    from builtins import print as _ab\n"
      "    for _ab_i in (0, 1):\n"
      "        _ab_x = _ab\n"
      "        _ab_x(md, 'common', object())\n"
      "        from builtins import setattr as _ab\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    # #1953 fix-r1 round 17 (codex r16 P0-1 / P0-2 / P0-3, boundary r16 P0-2):
    # the round-16 fix decided "is this name still the handle?" from the
    # positionally latest binding alone.  A conditional overwrite, a same-named
    # binding in a sibling function, and a constructor shadowed in the same
    # scope each hid a real write.  One row per spelling, injected into the
    # ruler's own source like the round-16 rows above.
    ('calibrate-for-target-handle-conditional-overwrite', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    def _cb_use(flag):\n"
      "        _cb = getattr(md, '__setattr__')\n"
      "        if flag:\n"
      "            _cb = lambda *a: None\n"
      "        _cb('common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-scoped-handle-receiver', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    def _sr_decoy():\n"
      "        _sr = getattr(SESSION_DOMAIN, '__setattr__')\n"
      "    def _sr_use():\n"
      "        _sr = getattr(md, '__setattr__')\n"
      "        _sr('common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-for-target-shadowed-constructor', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    def _sc_use():\n"
      "        _sc = getattr(md, '__setattr__')\n"
      "        object = lambda: _sc\n"
      "        _sc = object()\n"
      "        _sc('common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    # #1953 fix-r1 round 18 (codex r17 P0-1 / P0-2 / P1-3, GLM sibling r17 P2):
    # the round-17 relay guards read a container *copy* as a fresh value, the
    # status ruler decoded only a directly bound handle, and the freshness
    # guard ran only on two-argument calls.  One row per spelling; the first
    # is injected into the ruler's own source like the round-17 rows, the two
    # status rows replace a real status write in ``session_domain.py`` like
    # every other ``calibrate-*-status`` row.
    ('calibrate-for-target-handle-container-copy', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    def _hc_use():\n"
      "        _hc = [getattr(md, _k) for _k in ('__setattr__',)]\n"
      "        _hc = list(_hc)\n"
      "        for _h in _hc:\n"
      "            _h('common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    ('calibrate-status-bound-handle-alias-chain', SOURCE,
     "                    report['status'] = overall\n",
     "                    _g = getattr(report, '__setattr__')\n"
     "                    _f = _g\n"
     "                    _f('status', 'PASS')\n"),
    ('calibrate-status-three-arg-handle-live', SOURCE,
     "                    report['status'] = overall\n",
     "                    _h = setattr\n"
     "                    _h(report, 'status', 'PASS')\n"),
    # #1953 fix-r1 round 19 (GLM boundary r17 P0-1): the callee that produces
    # a bound handle may itself be an alias of ``getattr``.  The literal-only
    # decode read the two-argument call as the unbound builtin and the status
    # write vanished.
    ('calibrate-status-getattr-alias-callee', SOURCE,
     "                    report['status'] = overall\n",
     "                    _ga = getattr\n"
     "                    _f = _ga(report, '__setattr__')\n"
     "                    _f('status', 'PASS')\n"),
    # #1953 fix-r1 round 19 (codex r18 P0-1 / GLM boundary r18 P0-1): the
    # copy may be wrapped twice or sliced, and the one-level decoder read it
    # as a fresh value while the loop still called the handles.
    ('calibrate-for-target-nested-container-copy', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    def _nc_use():\n"
      "        _nc = [getattr(md, _k) for _k in ('__setattr__',)]\n"
      "        _nc2 = list(tuple(_nc))\n"
      "        for _h in _nc2:\n"
      "            _h('common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    # #1953 fix-r1 round 20 (codex r19 P0-1): the handle is copied into another
    # name *before* the source name is overwritten; resolving the hop at the
    # final call read the later ``print`` and the status write vanished.
    ('calibrate-status-alias-copied-before-overwrite', SOURCE,
     "                    report['status'] = overall\n",
     "                    _g = report.__setitem__\n"
     "                    _f = _g\n"
     "                    _g = print\n"
     "                    _f('status', 'PASS')\n"),
    # #1953 fix-r1 round 20 (codex r19 P0-2): a container rebound to a fresh
    # empty list and then filled in place still carries the handle to the copy.
    ('calibrate-for-target-container-filled-after-rebind', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    def _nc_use():\n"
      "        _nc = [getattr(md, _k) for _k in ('__setattr__',)]\n"
      "        _nc = []\n"
      "        _nc.append(getattr(md, '__setattr__'))\n"
      "        _nc2 = list(_nc)\n"
      "        for _h in _nc2:\n"
      "            _h('common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
    # #1953 fix-r1 round 21 (GLM boundary r19 P0-1 / sibling r19 P1-1): the
    # handle factory reaches the call through an import alias, so the status
    # write must still be seen.
    ('calibrate-status-imported-getattr-factory', SOURCE,
     "                    report['status'] = overall\n",
     "                    from builtins import getattr as _ga\n"
     "                    _f = _ga(report, '__setattr__')\n"
     "                    _f('status', 'PASS')\n"),
    # #1953 fix-r1 round 22 (codex r21 P0-1 / P0-2, GLM boundary r21 P0-1 /
    # P0-2): the builtins module itself reaches the call under an import
    # alias, and one assignment of another value must not hide the factory
    # spelling the import table records.
    ('calibrate-status-builtins-module-alias', SOURCE,
     "                    report['status'] = overall\n",
     "                    import builtins as _bi\n"
     "                    _f = _bi.getattr(report, '__setattr__')\n"
     "                    _f('status', 'PASS')\n"),
    ('calibrate-status-factory-import-after-assignment', SOURCE,
     "                    report['status'] = overall\n",
     "                    _ga = print\n"
     "                    from builtins import getattr as _ga\n"
     "                    _f = _ga(report, '__setattr__')\n"
     "                    _f('status', 'PASS')\n"),
    ('calibrate-status-factory-import-conditional-overwrite', SOURCE,
     "                    report['status'] = overall\n",
     "                    from builtins import getattr as _ga\n"
     "                    if overall:\n"
     "                        _ga = dict\n"
     "                    _f = _ga(report, '__setattr__')\n"
     "                    _f('status', 'PASS')\n"),
    # #1953 fix-r1 round 23 (GLM boundary r22 P1-2): ``bi = bi`` is a run-time
    # no-op, so the module reading has to survive it.
    ('calibrate-status-builtins-alias-self-assign', SOURCE,
     "                    report['status'] = overall\n",
     "                    import builtins as _bi\n"
     "                    _bi = _bi\n"
     "                    _f = _bi.getattr(report, '__setattr__')\n"
     "                    _f('status', 'PASS')\n"),
    # #1953 fix-r1 round 23 (GLM sibling r22 P2-3): the subscript spelling of
    # a module name copied from ``__builtins__`` is the same status write.
    ('calibrate-status-builtins-subscript-copy', SOURCE,
     "                    report['status'] = overall\n",
     "                    _bi = __builtins__\n"
     "                    _bi['setattr'](report, 'status', 'PASS')\n"),
    # #1953 fix-r1 round 21 (GLM sibling r19 P1-2 narrow positive): the copy
    # of a container that still carries handles stays a handle container when
    # it reaches the loop under a third name.
    ('calibrate-for-target-alias-of-container-copy', CHECK,
     "def load_session_domain():\n"
     "    spec = importlib.util.spec_from_file_location('session_domain', "
     "SESSION_DOMAIN)",
     (
      "if False:\n"
      "    def _nc_use():\n"
      "        _nc = [getattr(md, _k) for _k in ('__setattr__',)]\n"
      "        _nc2 = list(_nc)\n"
      "        _nc3 = _nc2\n"
      "        for _h in _nc3:\n"
      "            _h('common', object())\n"
      "\n"
      "\n"
      "\n"
      "def load_session_domain():\n"
      "    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)\n"
     )),
]


def calibrate_dict_namespace_update(rows):
    """#892 fix-r28: ``md.__dict__.update(common=<stub>)`` writes the mapping."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: namespace-dict update flagged, passed=False')


def calibrate_dict_namespace_augassign(rows):
    """#892 fix-r28: ``md.__dict__ |= {'common': <stub>}``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: namespace-dict in-place update flagged, passed=False')


def calibrate_dunder_setattr_receiver(rows):
    """#892 fix-r28: ``md.__setattr__('common', <stub>)``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: bound __setattr__ receiver flagged, passed=False')


def calibrate_object_setattr_unbound(rows):
    """#892 fix-r28: ``object.__setattr__(md, 'common', <stub>)``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: unbound __setattr__ receiver flagged, passed=False')


def calibrate_dict_alias_write(rows):
    """#892 fix-r28: a name bound to the receiver's ``__dict__``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: __dict__ alias write flagged, passed=False')


def calibrate_import_call_modules(rows):
    """#892 fix-r28: ``__import__('sys').modules[__name__]`` is a module object."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: __import__ module setattr flagged, passed=False')


def calibrate_dict_update_unbound(rows):
    """#892 fix-r29: ``dict.update(md.__dict__, common=<stub>)``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: unbound dict update flagged, passed=False')


def calibrate_dict_setdefault_unbound(rows):
    """#892 fix-r29: ``dict.setdefault(md.__dict__, 'common', <stub>)``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: unbound dict setdefault flagged, passed=False')


def calibrate_dict_pop_unbound(rows):
    """#892 fix-r29: ``dict.pop(md.__dict__, 'common', <stub>)``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: unbound dict pop flagged, passed=False')


def calibrate_dict_setitem_unbound(rows):
    """#892 fix-r29: ``dict.__setitem__(md.__dict__, 'common', <stub>)``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: unbound dict setitem flagged, passed=False')


def calibrate_helper_setattr(rows):
    """#892 fix-r29: ``attr.setattr(md, 'common', <stub>)``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: helper-module setattr flagged, passed=False')


def calibrate_operator_setitem(rows):
    """#892 fix-r30: ``operator.setitem(md.__dict__, 'common', <stub>)``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: operator.setitem write flagged, passed=False')


def calibrate_getattr_dunder_write(rows):
    """#892 fix-r30: ``getattr(md, '__setattr__')('common', <stub>)``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: getattr dunder write flagged, passed=False')


def calibrate_getattr_builtin_write(rows):
    """#892 fix-r30: ``getattr(builtins, 'setattr')(md, 'common', <stub>)``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: getattr builtin write flagged, passed=False')


def calibrate_partial_write(rows):
    """#892 fix-r30: ``functools.partial(setattr, md)('common', <stub>)``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: partial write flagged, passed=False')


def calibrate_partial_handle(rows):
    """#892 fix-r30: a named ``partial`` handle keeps the write callable."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: partial handle write flagged, passed=False')


def calibrate_tuple_attribute_write(rows):
    """#892 fix-r30: ``md.common, _x = None, None``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: tuple attribute write flagged, passed=False')


def calibrate_eval_literal_write(rows):
    """#892 fix-r30: a literal ``eval`` can write anything."""
    return _audit_needle(
        rows, f'{CHECK_REL} rebinds the shared aggregator module',
        f'aggregation-audit: {CHECK_REL} eval write flagged, passed=False')


def calibrate_exec_receiver_dict(rows):
    """#892 fix-r30: ``exec(src, md.__dict__)`` writes the receiver."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: exec receiver dict flagged, passed=False')


def calibrate_locals_write(rows):
    """#892 fix-r30: ``locals()['common'] = <stub>`` at module scope."""
    return _audit_needle(
        rows, f'{CHECK_REL} rebinds the shared aggregator module',
        f'aggregation-audit: {CHECK_REL} locals write flagged, passed=False')


def calibrate_namespace_unbound_update(rows):
    """#892 fix-r30: ``dict.update(globals(), common=<stub>)``."""
    return _audit_needle(
        rows, f'{CHECK_REL} rebinds the shared aggregator module',
        f'aggregation-audit: {CHECK_REL} unbound namespace update flagged, '
        'passed=False')


def calibrate_builtins_type_alias(rows):
    """#892 fix-r30: ``from builtins import dict as _bd`` names the type."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: builtins type alias flagged, passed=False')


def calibrate_dict_alias_augassign(rows):
    """#892 fix-r30: ``_d = md.__dict__; _d |= {'common': <stub>}``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: __dict__ alias augassign flagged, passed=False')


def calibrate_walrus_receiver_write(rows):
    """#892 fix-r30: ``(m := md); setattr(m, 'common', <stub>)``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: walrus receiver write flagged, passed=False')


def calibrate_alias_chain_reverse(rows):
    """#892 fix-r30: a reverse-order alias chain needs a real fixpoint."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: reverse alias chain flagged, passed=False')


def calibrate_sm_pair_value(rows):
    """#892 fix-r30: a loop pair's *value* variable is not the written value."""
    return _audit_needle(
        rows, f'{CONTROLS_REL} rebinds the shared aggregator module',
        f'aggregation-audit: {CONTROLS_REL} loop pair value flagged, '
        'passed=False')


def calibrate_sm_loop_rebind(rows):
    """#892 fix-r30: a loop key rebound in the body is unbounded."""
    return _audit_needle(
        rows, f'{CONTROLS_REL} rebinds the shared aggregator module',
        f'aggregation-audit: {CONTROLS_REL} sys.modules loop rebind flagged, '
        'passed=False')


def calibrate_sm_unbound_update(rows):
    """#892 fix-r30: ``dict.update(sys.modules, {'_guard': <stub>})``."""
    return _audit_needle(
        rows, f'{CONTROLS_REL} rebinds the shared aggregator module',
        f'aggregation-audit: {CONTROLS_REL} unbound sys.modules update '
        'flagged, passed=False')


def calibrate_getattr_dict_subscript(rows):
    """#892 fix-r30: ``getattr(md, '__dict__')['common'] = <stub>``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: getattr __dict__ subscript write flagged, '
        'passed=False')


def calibrate_builtins_subscript_setattr(rows):
    """#892 fix-r30: ``__builtins__['setattr'](md, 'common', <stub>)``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: __builtins__ subscript setattr flagged, '
        'passed=False')


def calibrate_getattr_import_alias(rows):
    """#892 fix-r31: ``from builtins import getattr as _g`` then ``_g(...)``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: aliased builtins getattr __dict__ write flagged, '
        'passed=False')


def calibrate_builtins_module_alias(rows):
    """#892 fix-r31: ``import builtins as _b; _b.getattr(md, '__dict__')``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: aliased builtins module getattr flagged, '
        'passed=False')


def calibrate_builtins_getattr_subscript(rows):
    """#892 fix-r31: ``__builtins__['getattr'](md, '__dict__')``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: __builtins__ subscript getattr flagged, '
        'passed=False')


def calibrate_builtins_subscript_vars(rows):
    """#892 fix-r31: ``__builtins__['vars'](md)``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: __builtins__ subscript vars flagged, '
        'passed=False')


def calibrate_status_getattr_alias(rows):
    """#892 fix-r32: a status write through a ``getattr`` alias."""
    return _calibrate_status_write_spelling(
        rows, "inline status/exit assignment _ga(report, '__setitem__')('status', 'PASS')")


def calibrate_write_handle_name(rows):
    """#892 fix-r32: a bound managed write kept in a plain name."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-write-handle-name flagged, passed=False')


def calibrate_write_handle_getattr(rows):
    """#892 fix-r32: a bound managed write kept in a name through ``getattr``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-write-handle-getattr flagged, passed=False')


def calibrate_write_handle_attrgetter(rows):
    """#892 fix-r32: a write handle built by ``operator.attrgetter``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-write-handle-attrgetter flagged, passed=False')


def calibrate_star_setattr(rows):
    """#892 fix-r32: a managed write whose arguments arrive in a star argument."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-star-setattr flagged, passed=False')


def calibrate_star_forward(rows):
    """#892 fix-r32: a managed write forwarded from ``*args``/``**kwargs``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-star-forward flagged, passed=False')


def calibrate_unpack_receiver(rows):
    """#892 fix-r32: a receiver handed on through an unpacked assignment target."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-unpack-receiver flagged, passed=False')


def calibrate_for_receiver(rows):
    """#892 fix-r32: a receiver bound by a ``for`` target."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-receiver flagged, passed=False')


def calibrate_default_receiver(rows):
    """#892 fix-r32: a receiver bound by a parameter default."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-default-receiver flagged, passed=False')


def calibrate_with_receiver(rows):
    """#892 fix-r32: a receiver bound by a ``with ... as`` target."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-with-receiver flagged, passed=False')


def calibrate_with_key_rebind(rows):
    """#892 fix-r32: a loop key rebound by a ``with ... as`` target."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-with-key-rebind flagged, passed=False')


def calibrate_globals_tuple_target(rows):
    """#892 fix-r32: a namespace write through an unpacked subscript target."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-globals-tuple-target flagged, passed=False')


def calibrate_sm_callable_setitem(rows):
    """#892 fix-r32: a ``sys.modules`` write through an aliased ``operator.setitem``.

    #892 fix-r34: the mutant poisoned the *fixture* file, where ``_guard`` is
    never read out of ``sys.modules`` -- the static ``common`` binding stays
    the shared module, so PASS was the right verdict and the mutant measured
    nothing.  It now spells the same write where the rule is live (the
    controls file reads ``_guard`` from ``sys.modules``).
    """
    return _audit_needle(
        rows, f'{CONTROLS_REL} rebinds the shared aggregator module',
        'aggregation-audit: calibrate-sm-callable-setitem flagged, passed=False')


def calibrate_sm_mapping_augassign(rows):
    """#892 fix-r32: a ``sys.modules`` write through an in-place mapping update.

    #892 fix-r34: same re-pointing as ``calibrate-sm-callable-setitem`` -- the
    fixture has no ``sys.modules.get`` read, so the poison has to land in the
    controls file to exercise the rule.
    """
    return _audit_needle(
        rows, f'{CONTROLS_REL} rebinds the shared aggregator module',
        'aggregation-audit: calibrate-sm-mapping-augassign flagged, passed=False')


def calibrate_builtins_alias_subscript(rows):
    """#892 fix-r32: a builtin read through an alias of the ``__builtins__`` table."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-builtins-alias-subscript flagged, passed=False')


def calibrate_getattr_dynamic_call(rows):
    """#892 fix-r32: a call through ``getattr`` with a non-literal attribute name."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-getattr-dynamic-call flagged, passed=False')


def calibrate_globals_alias_augassign(rows):
    """#892 fix-r32: a namespace replacement through an aliased ``globals()`` ``|=``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-globals-alias-augassign flagged, passed=False')


def calibrate_closure_default(rows):
    """#892 fix-r34: calibrate-closure-default."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-closure-default flagged, passed=False')


def calibrate_global_handoff(rows):
    """#892 fix-r34: calibrate-global-handoff."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-global-handoff flagged, passed=False')


def calibrate_star_from_name(rows):
    """#892 fix-r34: calibrate-star-from-name."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-star-from-name flagged, passed=False')


def calibrate_star_target_rest(rows):
    """#892 fix-r34: calibrate-star-target-rest."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-star-target-rest flagged, passed=False')


def calibrate_handle_annassign(rows):
    """#892 fix-r34: calibrate-handle-annassign."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-handle-annassign flagged, passed=False')


def calibrate_handle_argument(rows):
    """#892 fix-r34: calibrate-handle-argument."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-handle-argument flagged, passed=False')


def calibrate_ordered_binding(rows):
    """#892 fix-r34: calibrate-ordered-binding."""
    return _unrouted_status(rows, 'calibrate-ordered-binding')


def calibrate_partial_update_prebound(rows):
    """#892 fix-r34: calibrate-partial-update-prebound."""
    return _unrouted_status(rows, 'calibrate-partial-update-prebound')


def calibrate_partial_setdefault_prebound(rows):
    """#892 fix-r34: calibrate-partial-setdefault-prebound."""
    return _unrouted_status(rows, 'calibrate-partial-setdefault-prebound')


def calibrate_builtins_chain_fiftyone(rows):
    """#892 fix-r34: calibrate-builtins-chain-fiftyone."""
    return _unrouted_status(rows, 'calibrate-builtins-chain-fiftyone')


def calibrate_loop_backedge_alias(rows):
    """#1953 A: a loop back edge rebinds the alias after the use line, so the
    line-only binding filter still handed out the name."""
    return _unrouted_status(rows, 'calibrate-loop-backedge-alias')


def calibrate_sameline_restore(rows):
    """#1953 B: a same-line rebind and restore tied on ``(lineno, value)``, so
    the pick broke by walk order instead of source order."""
    return _unrouted_status(rows, 'calibrate-sameline-restore')


def calibrate_shared_seen_paths(rows):
    """#1953 C: one ``seen`` set shared by a name's sibling bindings let the
    first path shadow the second."""
    return _unrouted_status(rows, 'calibrate-shared-seen-paths')


def calibrate_decorator_lambda_scope(rows):
    """#1953 D: a decorator expression evaluated in the enclosing scope was
    attributed to the decorated function's scope."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-decorator-lambda-scope flagged, '
        'passed=False')


def calibrate_nonlocal_walrus_withdraw(rows):
    """#1953 E: the nonlocal withdrawal missed a ``NamedExpr`` (and match
    captures) written into the outer name."""
    return _must_call_shared(
        rows, '_failure',
        'aggregation-audit: calibrate-nonlocal-walrus-withdraw flagged, '
        'passed=False')


def calibrate_handle_list_literal(rows):
    """#1953 F1: a handle stored in a list literal escaped the escape test."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-handle-list-literal flagged, '
        'passed=False')


def calibrate_handle_dict_literal(rows):
    """#1953 F2: a handle stored in a dict literal escaped the escape test."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-handle-dict-literal flagged, '
        'passed=False')


def calibrate_handle_yield(rows):
    """#1953 F3: a handle handed out through ``yield`` escaped the escape
    test."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-handle-yield flagged, passed=False')


def calibrate_getattribute_handover(rows):
    """#1953 G: ``__getattribute__`` read by a non-literal name hands over the
    ``__dict__`` and was not in the read-handover set."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-getattribute-handover flagged, '
        'passed=False')


def calibrate_handle_dictcomp_escape(rows):
    """#1953 fix-r1 N: a dict comprehension carried the write handle past the
    escape test, so the receiver stayed trusted."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-handle-dictcomp-escape flagged, '
        'passed=False')


def calibrate_handle_listcomp_escape(rows):
    """#1953 fix-r1 N2: the list-comprehension spelling of the same escape."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-handle-listcomp-escape flagged, '
        'passed=False')


def calibrate_handle_aliased_escape(rows):
    """#1953 fix-r1: laundering one handle out of the container into a plain
    name (``f = hs[0]; f('common', <stub>)``) is the same hand-over."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-handle-aliased-escape flagged, '
        'passed=False')


def calibrate_getattribute_write_attribute(rows):
    """#1953 fix-r1 L (narrow positive): reading ``__doc__`` through
    ``__getattribute__`` is a pure read, but asking it for ``__setattr__``
    still hands the write over -- the pure-read exemption must not swallow
    that."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-getattribute-write-attribute flagged, '
        'passed=False')


def calibrate_getattr_const_subscript_write(rows):
    """#1953 fix-r1 M (narrow positive): folding a constant subscript must not
    blind the rule -- ``_keys = ('__setattr__',); getattr(md, _keys[0])`` is
    still the write handle, only the read spelling is exempt."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-getattr-const-subscript-write flagged, '
        'passed=False')


def calibrate_match_capture_helper(rows):
    """#1953 fix-r1 O: ``match`` pattern captures were absent from
    ``_name_bindings``, so ``case h:`` stayed unresolved."""
    return _unrouted_status(rows, 'calibrate-match-capture-helper')


def calibrate_comprehension_write_helper(rows):
    """#1953 fix-r1: a write performed inside a comprehension element is not a
    statement, so the per-statement scan never saw it."""
    return _unrouted_status(rows, 'calibrate-comprehension-write-helper')


def calibrate_mixed_alias_binding(rows):
    """#1953 fix-r1 P: a name with both a non-write and a write binding made
    the resolution non-unique and it was dropped instead of failing closed."""
    return _unrouted_status(rows, 'calibrate-mixed-alias-binding')


def calibrate_handle_container_alias(rows):
    """#1953 fix-r1 r1b: ``other = hs`` hands the whole container over, and
    ``other[0](...)`` takes the same handle out of the second name."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-handle-container-alias flagged, '
        'passed=False')


def calibrate_handle_mapping_write(rows):
    """#1953 fix-r1 r1b: the handle is not only called -- writing a mapping or
    attribute through ``hs[0]`` writes the object it was taken from."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-handle-mapping-write flagged, '
        'passed=False')


def calibrate_list_const_subscript_write(rows):
    """#1953 fix-r1 r1b: a list literal is mutable, so folding ``_keys[0]``
    into its initial value hid the write that replaced it."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-list-const-subscript-write flagged, '
        'passed=False')


def calibrate_comprehension_filter_write(rows):
    """#1953 fix-r1 r1b: the write sat in a comprehension filter, which is
    neither the element nor a statement."""
    return _unrouted_status(rows, 'calibrate-comprehension-filter-write')


def calibrate_match_capture_receiver(rows):
    """#1953 fix-r1 r1b: ``case h:`` also aliases the *receiver*, so
    ``h.common = <stub>`` writes the subject the pattern matched."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-match-capture-receiver flagged, '
        'passed=False')


def calibrate_match_capture_setattr_receiver(rows):
    """#1953 fix-r1 r1b: same capture, handed to ``setattr`` rather than
    written with an attribute store."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-match-capture-setattr-receiver flagged, '
        'passed=False')


def calibrate_getattribute_attr_spelling(rows):
    """#1953 fix-r1 r1b: ``md.__getattribute__('__dict__')`` is the
    attribute-spelling route to the same mapping ``getattr`` reaches."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-getattribute-attr-spelling flagged, '
        'passed=False')


def calibrate_getattribute_name_spelling(rows):
    """#1953 fix-r1 r2 (codex r2 P0): ``md.__getattribute__(_k)`` with ``_k``
    bound to ``'__dict__'`` names the same mapping as the literal spelling; an
    attribute name we cannot prove is a pure read must fail closed."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-getattribute-name-spelling flagged, '
        'passed=False')


def calibrate_for_wrapped_container(rows):
    """#1953 fix-r1 r1b: iterating ``list(hs)`` takes the handles out of the
    container just as iterating ``hs`` does."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-wrapped-container flagged, '
        'passed=False')


def calibrate_for_target_after_loop(rows):
    """#1953 fix-r1 r4 (GLM boundary r2 P0): the loop target outlives the
    loop, so using it as a handle after the ``for`` still writes ``md``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-after-loop flagged, '
        'passed=False')


def calibrate_for_slice(rows):
    """#1953 fix-r1 r4: ``for h in hs[:]`` copies the container but hands out
    the same handles."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-slice flagged, passed=False')


def calibrate_for_copy(rows):
    """#1953 fix-r1 r4: ``for h in hs.copy()`` hands out the same handles."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-copy flagged, passed=False')


def calibrate_comp_result_receiver(rows):
    """#1953 fix-r1 r4 (GLM boundary r2 P1): a comprehension that only relays
    its target builds a container of the receiver, so taking an element out of
    the result and writing it writes ``md``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-comp-result-receiver flagged, '
        'passed=False')


def calibrate_for_target_rebind_if(rows):
    """#1953 fix-r1 r5 (codex r3 P0): a conditional rebinding of the loop
    target must not end the scan -- ``if False: _h = None`` is the attacker's
    way to look like a cleanup while the handle stays live."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-rebind-if flagged, '
        'passed=False')


def calibrate_for_target_after_match(rows):
    """#1953 fix-r1 r5 (codex r3 P0): the loop target used in a ``match`` case
    after the loop still writes ``md``; the parent-chain scan missed it."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-after-match flagged, '
        'passed=False')


def calibrate_comp_dict_result(rows):
    """#1953 fix-r1 r5 (codex r3 P0): a dict comprehension relaying its target
    builds a mapping of the receiver."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-comp-dict-result flagged, passed=False')


def calibrate_comp_name_iter(rows):
    """#1953 fix-r1 r5 (codex r3 P0): the comprehension's iterable may be a
    name bound to a literal container."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-comp-name-iter flagged, passed=False')


def calibrate_for_target_return(rows):
    """#1953 fix-r1 r7 (GLM boundary r3 P1): returning the loop target hands
    the write handle to the caller."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-return flagged, passed=False')


def calibrate_for_target_alias(rows):
    """#1953 fix-r1 r7 (GLM boundary r3 P1): ``f = h; f('common', <stub>)``
    writes through the copy of the handle."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-alias flagged, passed=False')


def calibrate_starred_const_subscript(rows):
    """#1953 fix-r1 r7 (GLM boundary r3 P1): a tuple with a starred element has
    no static length, so its subscript cannot be folded to a known name."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-starred-const-subscript-write flagged, '
        'passed=False')


def calibrate_comp_handle_relay(rows):
    """#1953 fix-r1 r7 (GLM sibling r3 P2): a comprehension that relays a known
    handle container hands the same handles out of its result."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-comp-handle-relay flagged, '
        'passed=False')


def calibrate_for_target_finally_rebind(rows):
    """#1953 fix-r1 r8: a rebind after ``raise`` never runs; the finally call does."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-finally-rebind flagged, '
        'passed=False')


def calibrate_for_target_closure(rows):
    """#1953 fix-r1 r8: a nested definition closes over the loop target."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-closure flagged, passed=False')


def calibrate_for_target_shadowed_fresh(rows):
    """#1953 fix-r1 r8: a rebound builtin name hands the handle back."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-shadowed-fresh flagged, '
        'passed=False')


def calibrate_for_target_with_alias(rows):
    """#1953 fix-r1 r8: the loop target handed to a ``with`` binding."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-with-alias flagged, '
        'passed=False')


def calibrate_for_target_except_alias(rows):
    """#1953 fix-r1 r8: the loop target round-tripped through an exception."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-except-alias flagged, '
        'passed=False')


def calibrate_for_target_name_iter(rows):
    """#1953 fix-r1 r8: a for statement over a name-bound literal."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-name-iter flagged, '
        'passed=False')


def calibrate_comp_handle_relay_for(rows):
    """#1953 fix-r1 r8: a relayed handle container iterated by ``for``."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-comp-handle-relay-for flagged, '
        'passed=False')


def calibrate_comp_handle_relay_chain(rows):
    """#1953 fix-r1 r8: a handle container relayed through two comprehensions."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-comp-handle-relay-chain flagged, '
        'passed=False')


def calibrate_for_target_assign_call(rows):
    """#1953 fix-r1 round 9: calibrate-for-target-assign-call."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-assign-call flagged, '
        'passed=False')


def calibrate_for_target_def_default(rows):
    """#1953 fix-r1 round 9: calibrate-for-target-def-default."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-def-default flagged, '
        'passed=False')


def calibrate_for_target_readonly_call_leak(rows):
    """#1953 fix-r1 round 9: calibrate-for-target-readonly-call-leak."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-readonly-call-leak flagged, '
        'passed=False')


def calibrate_for_target_nested_param(rows):
    """#1953 fix-r1 round 9: calibrate-for-target-nested-param."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-nested-param flagged, '
        'passed=False')


def calibrate_for_target_compare_call_leak(rows):
    """#1953 fix-r1 round 9: calibrate-for-target-compare-call-leak."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-compare-call-leak flagged, '
        'passed=False')


def calibrate_for_target_outer_shadow_fresh(rows):
    """#1953 fix-r1 round 9: calibrate-for-target-outer-shadow-fresh."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-outer-shadow-fresh flagged, '
        'passed=False')


def calibrate_for_target_outer_shadow_print(rows):
    """#1953 fix-r1 round 9: calibrate-for-target-outer-shadow-print."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-outer-shadow-print flagged, '
        'passed=False')


def calibrate_for_target_ambiguous_literal(rows):
    """#1953 fix-r1 round 9: calibrate-for-target-ambiguous-literal."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-ambiguous-literal flagged, '
        'passed=False')


def calibrate_for_target_comp_element_call(rows):
    """#1953 fix-r1 round 9: calibrate-for-target-comp-element-call."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-comp-element-call flagged, '
        'passed=False')


def calibrate_for_target_call_attribute(rows):
    """#1953 fix-r1 round 10: calibrate-for-target-call-attribute."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-call-attribute flagged, '
        'passed=False')


def calibrate_for_target_call_attribute_alias(rows):
    """#1953 fix-r1 round 10: calibrate-for-target-call-attribute-alias."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-call-attribute-alias flagged, '
        'passed=False')


def calibrate_for_target_false_filter_write(rows):
    """#1953 fix-r1 round 10: calibrate-for-target-false-filter-write."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-false-filter-write flagged, '
        'passed=False')


def calibrate_for_target_class_body_clear(rows):
    """#1953 fix-r1 round 10: calibrate-for-target-class-body-clear."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-class-body-clear flagged, '
        'passed=False')


def calibrate_for_target_with_suppress(rows):
    """#1953 fix-r1 round 10: calibrate-for-target-with-suppress."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-with-suppress flagged, '
        'passed=False')


def calibrate_for_target_class_body_shadow(rows):
    """#1953 fix-r1 round 10: calibrate-for-target-class-body-shadow."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-class-body-shadow flagged, '
        'passed=False')


def calibrate_for_target_lambda_default(rows):
    """#1953 fix-r1 round 10: calibrate-for-target-lambda-default."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-lambda-default flagged, '
        'passed=False')


def calibrate_for_target_class_body_def(rows):
    """#1953 fix-r1 round 10: calibrate-for-target-class-body-def."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-class-body-def flagged, '
        'passed=False')


def calibrate_for_target_method_self(rows):
    """#1953 fix-r1 round 10: calibrate-for-target-method-self."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-method-self flagged, '
        'passed=False')


def calibrate_for_target_conditional_literal_rebind(rows):
    """#1953 fix-r1 round 11: calibrate-for-target-conditional-literal-rebind."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-conditional-literal-rebind '
        'flagged, passed=False')


def calibrate_for_target_enclosing_binding_after_use(rows):
    """#1953 fix-r1 round 11: calibrate-for-target-enclosing-binding-after-use."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: '
        'calibrate-for-target-enclosing-binding-after-use flagged, '
        'passed=False')


def calibrate_for_target_tuple_partial_alias(rows):
    """#1953 fix-r1 round 11: calibrate-for-target-tuple-partial-alias."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-tuple-partial-alias flagged, '
        'passed=False')


def calibrate_for_target_comp_partial_shadow(rows):
    """#1953 fix-r1 round 11: calibrate-for-target-comp-partial-shadow."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-comp-partial-shadow flagged, '
        'passed=False')


def calibrate_for_target_assert_msg(rows):
    """#1953 fix-r1 round 11: calibrate-for-target-assert-msg."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-assert-msg flagged, '
        'passed=False')


def calibrate_for_target_enclosing_call_between_rebinds(rows):
    """#1953 fix-r1 round 12: calibrate-for-target-enclosing-call-between-rebinds."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: '
        'calibrate-for-target-enclosing-call-between-rebinds flagged, '
        'passed=False')


def calibrate_for_target_class_body_compound(rows):
    """#1953 fix-r1 round 12: calibrate-for-target-class-body-compound."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-class-body-compound flagged, '
        'passed=False')


def calibrate_for_target_class_method_closure(rows):
    """#1953 fix-r1 round 12: calibrate-for-target-class-method-closure."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-class-method-closure flagged, '
        'passed=False')


def calibrate_for_target_class_body_shadowed_constructor(rows):
    """#1953 fix-r1 round 12: calibrate-for-target-class-body-shadowed-constructor."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: '
        'calibrate-for-target-class-body-shadowed-constructor flagged, '
        'passed=False')


def calibrate_for_target_module_import_alias(rows):
    """#1953 fix-r1 round 13: calibrate-for-target-module-import-alias."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-module-import-alias flagged, '
        'passed=False')


def calibrate_for_target_class_body_copy_alias(rows):
    """#1953 fix-r1 round 13: calibrate-for-target-class-body-copy-alias."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-class-body-copy-alias flagged, '
        'passed=False')


def calibrate_for_target_nested_class_method(rows):
    """#1953 fix-r1 round 13: calibrate-for-target-nested-class-method."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-nested-class-method flagged, '
        'passed=False')


def calibrate_for_target_conditional_rebind(rows):
    """#1953 fix-r1 round 14: calibrate-for-target-conditional-rebind."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-conditional-rebind flagged, '
        'passed=False')


def calibrate_for_target_loop_back_edge(rows):
    """#1953 fix-r1 round 14: calibrate-for-target-loop-back-edge."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-loop-back-edge flagged, '
        'passed=False')


def calibrate_for_target_same_line_rebind(rows):
    """#1953 fix-r1 round 14: calibrate-for-target-same-line-rebind."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-same-line-rebind flagged, '
        'passed=False')


def calibrate_for_target_class_block_alias(rows):
    """#1953 fix-r1 round 14: calibrate-for-target-class-block-alias."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-class-block-alias flagged, '
        'passed=False')


def calibrate_for_target_module_call_before_rebind(rows):
    """#1953 fix-r1 round 15: calibrate-for-target-module-call-before-rebind."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-module-call-before-rebind '
        'flagged, passed=False')


def calibrate_for_target_module_self_assignment(rows):
    """#1953 fix-r1 round 15: calibrate-for-target-module-self-assignment."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-module-self-assignment '
        'flagged, passed=False')


def calibrate_for_target_module_loop_first_use(rows):
    """#1953 fix-r1 round 15: calibrate-for-target-module-loop-first-use."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-module-loop-first-use '
        'flagged, passed=False')


def calibrate_for_target_duplicated_unpack_target(rows):
    """#1953 fix-r1 round 16: calibrate-for-target-duplicated-unpack-target."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-duplicated-unpack-target '
        'flagged, passed=False')


def calibrate_for_target_module_call_site_loop_edge(rows):
    """#1953 fix-r1 round 16: calibrate-for-target-module-call-site-loop-edge."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-module-call-site-loop-edge '
        'flagged, passed=False')


def calibrate_for_target_default_value_call(rows):
    """#1953 fix-r1 round 16: calibrate-for-target-default-value-call."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-default-value-call '
        'flagged, passed=False')


def calibrate_for_target_assignment_back_edge(rows):
    """#1953 fix-r1 round 16: calibrate-for-target-assignment-back-edge."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-assignment-back-edge '
        'flagged, passed=False')


def calibrate_for_target_handle_conditional_overwrite(rows):
    """#1953 fix-r1 round 17: calibrate-for-target-handle-conditional-overwrite."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-handle-conditional-overwrite '
        'flagged, passed=False')


def calibrate_for_target_scoped_handle_receiver(rows):
    """#1953 fix-r1 round 17: calibrate-for-target-scoped-handle-receiver."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-scoped-handle-receiver '
        'flagged, passed=False')


def calibrate_for_target_shadowed_constructor(rows):
    """#1953 fix-r1 round 17: calibrate-for-target-shadowed-constructor."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-shadowed-constructor '
        'flagged, passed=False')


def calibrate_for_target_handle_container_copy(rows):
    """#1953 fix-r1 round 18 (codex r17 P0-1): a constructor copy of a handle
    container keeps the handles, so iterating it still hands a write over."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-handle-container-copy '
        'flagged, passed=False')


def calibrate_status_bound_handle_alias_chain(rows):
    """#1953 fix-r1 round 18 (codex r17 P0-2): the status write is still a
    write when the bound handle reaches the call through a second name."""
    return _calibrate_status_write_spelling(
        rows, "inline status/exit assignment _f('status', 'PASS')")


def calibrate_status_three_arg_handle_live(rows):
    """#1953 fix-r1 round 18 (codex r17 P1-3 narrow positive): the freshness
    guard must not clear a three-argument alias that still holds ``setattr``."""
    return _calibrate_status_write_spelling(
        rows, "inline status/exit assignment _h(report, 'status', 'PASS')")


def calibrate_status_getattr_alias_callee(rows):
    """#1953 fix-r1 round 19 (GLM boundary r17 P0-1): the handle factory is
    the builtin ``getattr`` even when it reaches the call under another name."""
    return _calibrate_status_write_spelling(
        rows, "inline status/exit assignment _f('status', 'PASS')")


def calibrate_for_target_nested_container_copy(rows):
    """#1953 fix-r1 round 19 (codex r18 P0-1 / GLM boundary r18 P0-1): a
    container copied twice keeps the handles the original held."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-nested-container-copy '
        'flagged, passed=False')


def calibrate_status_alias_copied_before_overwrite(rows):
    """#1953 fix-r1 round 20 (codex r19 P0-1): the handle was copied into
    another name before the source name was overwritten; the copy still
    writes the status slot."""
    return _calibrate_status_write_spelling(
        rows, "inline status/exit assignment _f('status', 'PASS')")


def calibrate_for_target_container_filled_after_rebind(rows):
    """#1953 fix-r1 round 20 (codex r19 P0-2): a container rebound to a fresh
    empty list and then filled in place still hands the handle to the copy."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-container-filled-after-rebind '
        'flagged, passed=False')


def calibrate_status_imported_getattr_factory(rows):
    """#1953 fix-r1 round 21 (GLM boundary r19 P0-1 / sibling r19 P1-1): the
    handle factory is ``getattr`` even when it is imported under another name."""
    return _calibrate_status_write_spelling(
        rows, "inline status/exit assignment _f('status', 'PASS')")


def calibrate_for_target_alias_of_container_copy(rows):
    """#1953 fix-r1 round 21 (GLM sibling r19 P1-2 narrow positive): aliasing
    a copy of a container that still carries handles does not make it fresh."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-alias-of-container-copy '
        'flagged, passed=False')


def calibrate_status_builtins_module_alias(rows):
    """#1953 fix-r1 round 22 (codex r21 P0-1 / GLM boundary r21 P0-2): the
    builtins module imported under another spelling is still the factory."""
    return _calibrate_status_write_spelling(
        rows, "inline status/exit assignment _f('status', 'PASS')")


def calibrate_status_factory_import_after_assignment(rows):
    """#1953 fix-r1 round 22 (codex r21 P0-2): an assignment of another value
    before the factory import must not hide the import event."""
    return _calibrate_status_write_spelling(
        rows, "inline status/exit assignment _f('status', 'PASS')")


def calibrate_status_factory_import_conditional_overwrite(rows):
    """#1953 fix-r1 round 22 (GLM boundary r21 P0-1): a conditional overwrite
    does not erase the factory spelling the import put in force."""
    return _calibrate_status_write_spelling(
        rows, "inline status/exit assignment _f('status', 'PASS')")


def calibrate_status_builtins_alias_self_assign(rows):
    """#1953 fix-r1 round 23 (GLM boundary r22 P1-2): ``bi = bi`` keeps the
    module reading the import put in force."""
    return _calibrate_status_write_spelling(
        rows, "inline status/exit assignment _f('status', 'PASS')")


def calibrate_status_builtins_subscript_copy(rows):
    """#1953 fix-r1 round 23 (GLM sibling r22 P2-3): the subscript spelling of
    a module name copied from ``__builtins__`` writes the same slot."""
    return _calibrate_status_write_spelling(
        rows, "inline status/exit assignment _bi['setattr'](report, 'status', 'PASS')")


def calibrate_for_target_nested_copy_alias(rows):
    """#1953 fix-r1 round 14: calibrate-for-target-nested-copy-alias."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-for-target-nested-copy-alias flagged, '
        'passed=False')


def _unrouted_status(rows, name):
    """#892 fix-r33: a status write the slot rule no longer sees as shared."""
    ok, detail = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, detail
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    needle = ('aggregation-audit: scripts/gateway-parity/session_domain.py:main '
              'must route every batch status/exit computation through '
              'common.aggregate_status')
    if not any(needle in failure for failure in (row.get('failures') or [])):
        return False, (f'failures={row.get("failures")!r}, want the {needle!r} '
                       'failure')
    return True, f'aggregation-audit: {name} flagged, passed=False'


def calibrate_global_walrus_rebind(rows):
    """#892 fix-r33: global walrus rebind."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-global-walrus-rebind flagged, passed=False')

def calibrate_unpack_star_target(rows):
    """#892 fix-r33: unpack star target."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-unpack-star-target flagged, passed=False')

def calibrate_unpack_star_value(rows):
    """#892 fix-r33: unpack star value."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-unpack-star-value flagged, passed=False')

def calibrate_unpack_iter_for(rows):
    """#892 fix-r33: unpack iter for."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-unpack-iter-for flagged, passed=False')

def calibrate_with_handle_name(rows):
    """#892 fix-r33: with handle name."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-with-handle-name flagged, passed=False')

def calibrate_getattr_handle_name(rows):
    """#892 fix-r33: getattr handle name."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-getattr-handle-name flagged, passed=False')

def calibrate_getattr_handle_dict(rows):
    """#892 fix-r33: getattr handle dict."""
    return _not_shared(
        rows, CHECK_REL, 'main',
        'aggregation-audit: calibrate-getattr-handle-dict flagged, passed=False')

def calibrate_nonlocal_common_rebind(rows):
    """#892 fix-r33: nonlocal common rebind."""
    return _unrouted_status(rows, 'calibrate-nonlocal-common-rebind')

def calibrate_builtin_last_binding(rows):
    """#892 fix-r33: builtin last binding."""
    return _unrouted_status(rows, 'calibrate-builtin-last-binding')

def calibrate_partial_prebound(rows):
    """#892 fix-r33: partial prebound."""
    return _unrouted_status(rows, 'calibrate-partial-prebound')

def calibrate_partial_name_status(rows):
    """#892 fix-r33: partial name status."""
    return _unrouted_status(rows, 'calibrate-partial-name-status')

def calibrate_builtins_chain_four(rows):
    """#892 fix-r33: builtins chain four."""
    return _unrouted_status(rows, 'calibrate-builtins-chain-four')

def linux_unknown_error_accepted(rows):
    matched=[row for row in rows if row.get('mode')=='blocked']
    if len(matched)!=1:return False,'blocked row missing'
    row=matched[0];observed_cases=row.get('cases') or []
    ok=(row.get('passed') is False and row.get('error') is None and len(observed_cases)==2
        and observed_cases[0].get('case')=='permission-denied' and observed_cases[0].get('processTreeCleanup')=='BLOCKED'
        and observed_cases[1].get('case')=='oserror-denied' and observed_cases[1].get('processTreeCleanup')=='VERIFIED'
        and observed_cases[1].get('topLevel')=='PASS')
    return ok,'generic OSError incorrectly became VERIFIED/PASS after permission denial stayed BLOCKED'


def linux_summary_forced_pass(rows):
    ok,detail=match(rows,'aggregation-audit',{'passed':False})
    if not ok:return ok,detail
    row,error=_audit_row(rows)
    if row is None:return False,error
    failures=row.get('failures') or []
    prefix='#892 aggregation-audit: src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/check_linux.py:summarize_controls '
    if not any(item.startswith(prefix) for item in failures):return False,repr(failures)
    return True,'the new Linux summary bypass was found at its own registered function'


def linux_kernel_regression(rows,case,field,want):
    matched=[row for row in rows if row.get('mode')=='linux-kernel-controls']
    if len(matched)!=1 or matched[0].get('passed') is not False:return False,'Linux kernel control must fail'
    report=json.loads(Path(matched[0]['report']).read_text())
    found=[row for row in report.get('rows',[])if row.get('case')==case]
    if len(found)!=1 or dig(found[0],field)!=want:return False,repr(found)
    if report.get('errors') or not report.get('failures'):return False,'must be a test assertion failure, not a crash'
    return True,f'{case}: {field}={want!r}, own control assertion failed'


class MissingKernelBranchEvidence(ValueError):
    """A requested kernel branch was not actually exercised."""


def kernel_lease_evidence(rows,label):
    matches=[row for row in rows if row.get('mode')=='kernel-lease']
    if len(matches)!=1:
        raise MissingKernelBranchEvidence(f'{label}: missing or duplicate lease control result')
    row=matches[0]
    proof=row.get('parentProof')
    if (not isinstance(proof,dict) or any(type(proof.get(k)) is not int or proof[k]<=0
                                          for k in ('parentPid','treeLockInode'))
            or row.get('positiveRestored') is not True or row.get('branchControls') not in ('PASS','FAIL')
            or type(row.get('passed')) is not bool):
        raise MissingKernelBranchEvidence(f'{label}: no valid real parent source lease')
    return row


def kernel_lease_branch_accepted(rows,branch):
    row=kernel_lease_evidence(rows,branch)
    branches=row.get('branches')
    observed=branches.get(branch) if isinstance(branches,dict) else None
    if not isinstance(observed,dict) or type(observed.get('accepted')) is not bool or 'reason' not in observed:
        raise MissingKernelBranchEvidence(f'{branch}: no valid real parent-lease injection result')
    accepted=(observed['accepted'] is True and observed['reason'] is None and row.get('passed') is False)
    return accepted, f'{branch}: accepted={observed["accepted"]!r}; reason={observed["reason"]!r}'


def kernel_lease_gate_removed(rows):
    row=kernel_lease_evidence(rows,'gate')
    if any(type(row.get(key)) is not bool for key in ('unownedPassed','foreignPassed')):
        raise MissingKernelBranchEvidence('gate: both unowned and wrong-owner controls must execute')
    return match(rows,'kernel-lease',{'unownedPassed':True,'foreignPassed':True})


JUDGES = {
    'kernel-source-lease-gate-removed':('kernel-lease',kernel_lease_gate_removed,False,()),
    'kernel-source-lease-cleanup-pending-guard-removed':('kernel-lease',lambda rows:kernel_lease_branch_accepted(rows,'cleanupPending'),False,()),
    'kernel-source-lease-token-guard-removed':('kernel-lease',lambda rows:kernel_lease_branch_accepted(rows,'token'),False,()),
    'kernel-source-lease-timestamp-guard-removed':('kernel-lease',lambda rows:kernel_lease_branch_accepted(rows,'timestamp'),False,()),
    'kernel-source-lease-flock-guard-removed':('kernel-lease',lambda rows:kernel_lease_branch_accepted(rows,'flock'),False,()),
    'kernel-result-passed-forced':('kernel-result',lambda rows:match(rows,'kernel-result',{'failedPacketPassed':True}),False,()),
    'empty-eligible-guard-removed':('batch-aggregation',lambda rows:match(rows,'batch-aggregation',{'negativeRestored.emptyStatus':'PASS','negativeRestored.emptyReturncode':0,'negativeRestored.truncatedStatus':'PASS','negativeRestored.truncatedReturncode':0,'negativeRestored.duplicateStatus':'PASS','negativeRestored.duplicateReturncode':0,'negativeRestored.renamedStatus':'PASS','negativeRestored.renamedReturncode':0,'negativeRestored.reorderedStatus':'PASS','negativeRestored.reorderedReturncode':0}),False,()),
    'linux-initial-identity-gate-removed':('linux-kernel-controls',lambda rows:linux_kernel_regression(rows,'helper-initial-identity','observerCalls',1),False,()),
    'linux-pre-reap-identity-gate-removed':('linux-kernel-controls',lambda rows:linux_kernel_regression(rows,'helper-pre-reap-identity','result.status','VERIFIED'),False,()),
    'linux-refusal-erases-finding':('linux-kernel-controls',lambda rows:linux_kernel_regression(rows,'signal-refused-positive-finding','result.status','NOT_VERIFIED'),False,()),
    'linux-empty-group-probe-removed':('clean',lambda rows:match(rows,'clean',{'processTreeCleanup':'GROUP_NOT_EMPTY','probe.status':'GROUP_NOT_EMPTY'}),True,()),
    'linux-sigterm-removed':('leak',lambda rows:match(rows,'leak',{'processTreeCleanup':'GROUP_NOT_EMPTY','probe.signal':'SIGKILL'}),True,()),
    'linux-sigkill-escalation-removed':('ignore',lambda rows:match(rows,'ignore',{'processTreeCleanup':'GROUP_NOT_EMPTY','probe.signal':'SIGTERM','probe.emptyAfterSignal':False}),True,()),
    'linux-positive-finding-dropped-on-denial':('eperm-live-member',lambda rows:match(rows,'eperm-live-member',{'processTreeCleanup':'VERIFIED','topLevel':'PASS'}),False,()),
    'linux-unknown-error-means-empty':('blocked',linux_unknown_error_accepted,False,()),
    'linux-controls-summary-forced-pass':('aggregation-audit',linux_summary_forced_pass,False,()),
    'empty-group-probe-removed': (
        'clean',
        lambda rows: match(rows, 'clean', {
            'processTreeCleanup': 'GROUP_NOT_EMPTY',
            'probe.status': 'GROUP_NOT_EMPTY',
        }),
        True,
        ('signalError', 'probeError')),
    'cleanup-status-write-removed': (
        'leak',
        lambda rows: match(rows, 'leak', {
            'processTreeCleanup': 'VERIFIED',
            'probe.status': 'GROUP_NOT_EMPTY',
        }),
        True,
        ()),
    'top-level-mapping-relaxed': ('status', status_mapping, False, ()),
    'sigterm-removed': (
        'leak',
        lambda rows: match(rows, 'leak', {
            'processTreeCleanup': 'GROUP_NOT_EMPTY',
            'probe.signal': 'SIGKILL',
        }),
        True,
        ()),
    'blocked-branch-removed': ('blocked', blocked_branch_removed, False, ()),
    'sigkill-escalation-removed': (
        'ignore',
        lambda rows: match(rows, 'ignore', {
            'processTreeCleanup': 'GROUP_NOT_EMPTY',
            'probe.signal': 'SIGTERM',
            'probe.emptyAfterSignal': False,
        }),
        True,
        ()),
    'top-level-wiring-relaxed': ('wiring', wiring_relaxed, False, ()),
    'eperm-census-ignored': (
        'eperm-live-member',
        lambda rows: match(rows, 'eperm-live-member', {
            'processTreeCleanup': 'VERIFIED',
            'topLevel': 'PASS',
        }),
        False,
        ()),
    'unavailable-census-as-empty': (
        'census-unavailable',
        lambda rows: match(rows, 'census-unavailable', {
            'processTreeCleanup': 'VERIFIED',
            'topLevel': 'PASS',
        }),
        False,
        ()),
    'reap-guard-removed': ('reap-state', lambda rows: match(rows, 'reap-state', {'killCalls': 1}), False, ()),
    'snapshot-race-accepted': (
        'census-snapshot',
        lambda rows: match(rows, 'census-snapshot', {
            'processTreeCleanup': 'VERIFIED',
            'topLevel': 'PASS',
        }),
        False,
        ()),
    'unstable-census-accepted': (
        'census-unstable',
        lambda rows: match(rows, 'census-unstable', {'members': [900, 424245]}), False, ()),
    'truncation-accepted': (
        'census-truncated',
        lambda rows: match(rows, 'census-truncated', {'returnedPids': 1}), False, ()),
    'enumeration-failure-empty': (
        'census-enumeration',
        lambda rows: match(rows, 'census-enumeration', {
            'processTreeCleanup': 'VERIFIED',
            'topLevel': 'PASS',
        }),
        False,
        ()),
    'signal-gate-removed': (
        'signal-entry',
        lambda rows: match(rows, 'signal-entry', {
            'groupOutcome': 'denied',
            'leaderOutcome': 'denied',
            'killCalls': 2,
        }),
        False,
        ()),
    'pid-blocked-as-gone': (
        'pid-blocked',
        lambda rows: match(rows, 'pid-blocked', {
            'grandchildState': 'missing',
            'grandchildAlive': False,
            'grandchildGone': True,
        }),
        False,
        ()),
    'linux-visible-census-trusted': (
        'linux-filtered-proc',
        lambda rows: match(rows, 'linux-filtered-proc', {
            'processTreeCleanup': 'VERIFIED',
            'topLevel': 'PASS',
        }),
        False,
        ()),
    'linux-eperm-means-empty': (
        'linux-filtered-proc',
        lambda rows: match(rows, 'linux-filtered-proc', {'processTreeCleanup': 'VERIFIED', 'topLevel': 'PASS'}),
        False, ()),
    'linux-no-esrch-needed': (
        'linux-filtered-proc',
        lambda rows: match(rows, 'linux-filtered-proc', {'aliveProbe.status': 'VERIFIED'}),
        False, ()),
    'batch-status-relaxed': (
        'batch-aggregation',
        lambda rows: match(rows, 'batch-aggregation', {
            'withSkips.status': 'PASS',
            'withSkips.returncode': 0,
            'blocked.status': 'PASS',
            'blocked.returncode': 0,
            'notRun.status': 'PASS',
            'notRun.returncode': 0,
        }),
        False,
        ()),
    'fixture-summary-relaxed': (
        'batch-aggregation',
        lambda rows: match(rows, 'batch-aggregation', {
            'summaries.withSkip': 'FAIL',
            'summaries.allSkipped': 'FAIL',
        }),
        False,
        ()),
    'calibrate-controls-skip-accepted': (
        'batch-aggregation',
        lambda rows: match(rows, 'batch-aggregation', {
            'calibrateSkipped.status': 'PASS',
            'calibrateSkipped.failureRule': None,
            'calibrateSkipped.returncode': 0,
        }),
        False,
        ()),
    'aggregation-exit-bypassed': (
        'aggregation-audit',
        lambda rows: match(rows, 'aggregation-audit', {
            'firstFailure': '#164-fix-r7 aggregation-audit: '
                            'scripts/gateway-parity/controls/session_domain.py:main '
                            'must call common.aggregate_status',
            'points.controls-main.shared': False,
        }),
        False,
        ()),
    'summary-dict-bypass': ('aggregation-audit', summary_dict_bypass, False, ()),
    'aggregation-branch-exit-inline': ('aggregation-audit', branch_exit_inline, False, ()),
    'summary-name-bypass': ('aggregation-audit', summary_name_bypass, False, ()),
    'bare-exit-zero': ('aggregation-audit', bare_exit_zero, False, ()),
    'status-slot-relayed': ('aggregation-audit', status_slot_relayed, False, ()),
    'system-exit-zero-bypass': ('aggregation-audit', system_exit_zero_bypass, False, ()),
    'bare-return-exit': ('aggregation-audit', bare_return_exit, False, ()),
    'conditional-arm-exit': ('aggregation-audit', conditional_arm_exit, False, ()),
    'calibrate-bare-zero': ('aggregation-audit', calibrate_bare_zero, False, ()),
    'calibrate-status-write-pass': ('aggregation-audit', calibrate_status_write_pass, False, ()),
    'controls-return-none': (
        'aggregation-audit', controls_return_none, False, ()),
    'controls-system-exit-none': (
        'aggregation-audit', controls_system_exit_none, False, ()),
    'calibrate-exit-code-relayed': (
        'aggregation-audit', calibrate_exit_code_relayed, False, ()),
    'controls-status-slot-subscript': (
        'aggregation-audit', controls_status_slot_subscript, False, ()),
    'controls-aggregate-lookalike': (
        'aggregation-audit', controls_aggregate_lookalike, False, ()),
    'calibrate-status-update': (
        'aggregation-audit', calibrate_status_update, False, ()),
    'retirement-duplicate-status-key': (
        'aggregation-audit', retirement_duplicate_status_key, False, ()),
    'controls-nested-same-name': (
        'aggregation-audit', controls_nested_same_name, False, ()),
    'calibrate-shared-excuse': (
        'aggregation-audit', calibrate_shared_excuse, False, ()),
    'calibrate-status-setdefault': (
        'aggregation-audit', calibrate_status_setdefault, False, ()),
    'calibrate-early-return-branch': (
        'aggregation-audit', calibrate_early_return_branch, False, ()),
    'calibrate-two-hop-zero': (
        'aggregation-audit', calibrate_two_hop_zero, False, ()),
    'calibrate-int-call-code': (
        'aggregation-audit', calibrate_int_call_code, False, ()),
    'calibrate-dict-code-zero': (
        'aggregation-audit', calibrate_dict_code_zero, False, ()),
    'calibrate-nested-def-zero': (
        'aggregation-audit', calibrate_nested_def_zero, False, ()),
    'calibrate-sys-exit-zero': (
        'aggregation-audit', calibrate_sys_exit_zero, False, ()),
    'calibrate-walrus-status': (
        'aggregation-audit', calibrate_walrus_status, False, ()),
    'calibrate-rebuilt-summary': (
        'aggregation-audit', calibrate_rebuilt_summary, False, ()),
    'calibrate-verdict-write': (
        'aggregation-audit', calibrate_verdict_write, False, ()),
    'calibrate-boolean-exit': (
        'aggregation-audit', calibrate_boolean_exit, False, ()),
    'calibrate-sys-exit-relay': (
        'aggregation-audit', calibrate_sys_exit_relay, False, ()),
    'calibrate-system-exit-bare': (
        'aggregation-audit', calibrate_system_exit_bare, False, ()),
    'calibrate-delegated-chain': (
        'aggregation-audit', calibrate_delegated_chain, False, ()),
    'calibrate-container-slot': (
        'aggregation-audit', calibrate_container_slot, False, ()),
    'calibrate-alias-cycle': (
        'aggregation-audit', calibrate_alias_cycle, False, ()),
    'calibrate-verdict-dict': (
        'aggregation-audit', calibrate_verdict_dict, False, ()),
    'calibrate-conditional-summary': (
        'aggregation-audit', calibrate_conditional_summary, False, ()),
    'calibrate-update-expansion': (
        'aggregation-audit', calibrate_update_expansion, False, ()),
    'calibrate-exit-conditional': (
        'aggregation-audit', calibrate_exit_conditional, False, ()),
    'calibrate-return-exit-call': (
        'aggregation-audit', calibrate_return_exit_call, False, ()),
    'calibrate-ann-rebuilt': (
        'aggregation-audit', calibrate_ann_rebuilt, False, ()),
    'calibrate-bare-verdict': (
        'aggregation-audit', calibrate_bare_verdict, False, ()),
    'calibrate-bool-exit': (
        'aggregation-audit', calibrate_bool_exit, False, ()),
    'calibrate-bool-relay': (
        'aggregation-audit', calibrate_bool_relay, False, ()),
    'calibrate-unbound-exit-name': (
        'aggregation-audit', calibrate_unbound_exit_name, False, ()),
    'calibrate-update-positional': (
        'aggregation-audit', calibrate_update_positional, False, ()),
    'calibrate-augassign-update': (
        'aggregation-audit', calibrate_augassign_update, False, ()),
    'calibrate-tuple-verdict': (
        'aggregation-audit', calibrate_tuple_verdict, False, ()),
    'calibrate-cleanup-word': (
        'aggregation-audit', calibrate_cleanup_word, False, ()),
    'calibrate-unbound-exit-relay': (
        'aggregation-audit', calibrate_unbound_exit_relay, False, ()),
    'calibrate-no-exit-path': (
        'aggregation-audit', calibrate_no_exit_path, False, ()),
    'calibrate-delegated-attribute': (
        'aggregation-audit', calibrate_delegated_attribute, False, ()),
    'calibrate-opaque-clean-write': (
        'aggregation-audit', calibrate_opaque_clean_write, False, ()),
    'calibrate-opaque-update-payload': (
        'aggregation-audit', calibrate_opaque_update_payload, False, ()),
    'calibrate-param-augassign': (
        'aggregation-audit', calibrate_param_augassign, False, ()),
    'calibrate-param-update': (
        'aggregation-audit', calibrate_param_update, False, ()),
    'calibrate-helper-local-relay': (
        'aggregation-audit', calibrate_helper_local_relay, False, ()),
    'calibrate-helper-bare-sysexit': (
        'aggregation-audit', calibrate_helper_bare_sysexit, False, ()),
    'calibrate-attr-head-callee': (
        'aggregation-audit', calibrate_attr_head_callee, False, ()),
    'calibrate-mixed-branch-helper': (
        'aggregation-audit', calibrate_mixed_branch_helper, False, ()),
    'calibrate-cross-module-name': (
        'aggregation-audit', calibrate_cross_module_name, False, ()),
    'calibrate-exempt-callee-write': (
        'aggregation-audit', calibrate_exempt_callee_write, False, ()),
    'calibrate-quit-exit': (
        'aggregation-audit', calibrate_quit_exit, False, ()),
    'calibrate-partial-raise-point': (
        'aggregation-audit', calibrate_partial_raise_point, False, ()),
    'calibrate-param-augassign-opaque': (
        'aggregation-audit', calibrate_param_augassign_opaque, False, ()),
    'calibrate-augassign-cross-file-call': (
        'aggregation-audit', calibrate_augassign_cross_file_call, False, ()),
    'calibrate-exit-call-unregistered': (
        'aggregation-audit', calibrate_exit_call_unregistered, False, ()),
    'calibrate-tuple-status-target': (
        'aggregation-audit', calibrate_tuple_status_target, False, ()),
    'calibrate-attr-collision': (
        'aggregation-audit', calibrate_attr_collision, False, ()),
    'calibrate-bare-raise-exit': (
        'aggregation-audit', calibrate_bare_raise_exit, False, ()),
    'calibrate-alias-sysexit': (
        'aggregation-audit', calibrate_alias_sysexit, False, ()),
    'calibrate-alias-system-exit': (
        'aggregation-audit', calibrate_alias_system_exit, False, ()),
    'calibrate-setattr-status': (
        'aggregation-audit', calibrate_setattr_status, False, ()),
    'calibrate-dunder-setitem-status': (
        'aggregation-audit', calibrate_dunder_setitem_status, False, ()),
    'calibrate-local-import-alias': (
        'aggregation-audit', calibrate_local_import_alias, False, ()),
    'calibrate-module-alias-binding': (
        'aggregation-audit', calibrate_module_alias_binding, False, ()),
    'calibrate-attr-status': (
        'aggregation-audit', calibrate_attr_status, False, ()),
    'calibrate-dunder-setattr-status': (
        'aggregation-audit', calibrate_dunder_setattr_status, False, ()),
    'calibrate-unbound-setitem-status': (
        'aggregation-audit', calibrate_unbound_setitem_status, False, ()),
    'calibrate-builtins-setattr-status': (
        'aggregation-audit', calibrate_builtins_setattr_status, False, ()),
    'calibrate-operator-setitem-status': (
        'aggregation-audit', calibrate_operator_setitem_status, False, ()),
    'calibrate-try-assign-alias': (
        'aggregation-audit', calibrate_try_assign_alias, False, ()),
    'calibrate-attr-exit-code': (
        'aggregation-audit', calibrate_attr_exit_code, False, ()),
    'calibrate-subscript-exit-code': (
        'aggregation-audit', calibrate_subscript_exit_code, False, ()),
    'calibrate-attr-code-relay': (
        'aggregation-audit', calibrate_attr_code_relay, False, ()),
    'calibrate-try-else-completes': (
        'aggregation-audit', calibrate_try_else_completes, False, ()),
    'calibrate-exit-call-attribute': (
        'aggregation-audit', calibrate_exit_call_attribute, False, ()),
    'calibrate-exit-call-subscript': (
        'aggregation-audit', calibrate_exit_call_subscript, False, ()),
    'calibrate-shadowed-common': (
        'aggregation-audit', calibrate_shadowed_common, False, ()),
    'calibrate-computed-status-write': (
        'aggregation-audit', calibrate_computed_status_write, False, ()),
    'calibrate-local-exit-alias-chain': (
        'aggregation-audit', calibrate_local_exit_alias_chain, False, ()),
    'calibrate-computed-status-key': (
        'aggregation-audit', calibrate_computed_status_key, False, ()),
    'calibrate-computed-status-key-write': (
        'aggregation-audit', calibrate_computed_status_key_write, False, ()),
    'calibrate-shadowed-common-local': (
        'aggregation-audit', calibrate_shadowed_common_local, False, ()),
    'calibrate-shadowed-receiver-local': (
        'aggregation-audit', calibrate_shadowed_receiver_local, False, ()),
    'calibrate-shadowed-common-global': (
        'aggregation-audit', calibrate_shadowed_common_global, False, ()),
    'calibrate-shadowed-receiver-global': (
        'aggregation-audit', calibrate_shadowed_receiver_global, False, ()),
    'calibrate-shadowed-receiver-poison-fn': (
        'aggregation-audit', calibrate_shadowed_receiver_poison_fn, False, ()),
    'calibrate-shadowed-receiver-same-fn': (
        'aggregation-audit', calibrate_shadowed_receiver_same_fn, False, ()),
    'calibrate-redefined-loader': (
        'aggregation-audit', calibrate_redefined_loader, False, ()),
    'calibrate-conditional-def': (
        'aggregation-audit', calibrate_conditional_def, False, ()),
    'calibrate-decorated-source': (
        'aggregation-audit', calibrate_decorated_source, False, ()),
    'calibrate-nested-def-rebind': (
        'aggregation-audit', calibrate_nested_def_rebind, False, ()),
    'calibrate-walrus-rebind': (
        'aggregation-audit', calibrate_walrus_rebind, False, ()),
    'calibrate-dynamic-globals-write': (
        'aggregation-audit', calibrate_dynamic_globals_write, False, ()),
    'calibrate-sys-modules-poison': (
        'aggregation-audit', calibrate_sys_modules_poison, False, ()),
    'calibrate-shadowed-global-loader': (
        'aggregation-audit', calibrate_shadowed_global_loader, False, ()),
    'calibrate-module-walrus': (
        'aggregation-audit', calibrate_module_walrus, False, ()),
    'calibrate-dynamic-receiver-write': (
        'aggregation-audit', calibrate_dynamic_receiver_write, False, ()),
    'calibrate-setattr-shared-write': (
        'aggregation-audit', calibrate_setattr_shared_write, False, ()),
    'calibrate-exec-nonliteral': (
        'aggregation-audit', calibrate_exec_nonliteral, False, ()),
    'calibrate-negative-process-redefine': (
        'aggregation-audit', calibrate_negative_process_redefine, False, ()),
    'calibrate-decorated-guard-load': (
        'aggregation-audit', calibrate_decorated_guard_load, False, ()),
    'calibrate-direct-attribute-write': (
        'aggregation-audit', calibrate_direct_attribute_write, False, ()),
    'calibrate-dict-mutation-keys': (
        'aggregation-audit', calibrate_dict_mutation_keys, False, ()),
    'calibrate-sys-modules-wildcard': (
        'aggregation-audit', calibrate_sys_modules_wildcard, False, ()),
    'calibrate-single-definition-dynamic': (
        'aggregation-audit', calibrate_single_definition_dynamic, False, ()),
    'calibrate-dynamic-shared-name': (
        'aggregation-audit', calibrate_dynamic_shared_name, False, ()),
    'calibrate-builtin-alias-setattr': (
        'aggregation-audit', calibrate_builtin_alias_setattr, False, ()),
    'calibrate-vars-dict-receiver': (
        'aggregation-audit', calibrate_vars_dict_receiver, False, ()),
    'calibrate-loop-key-rebind': (
        'aggregation-audit', calibrate_loop_key_rebind, False, ()),
    'calibrate-definition-header-walrus': (
        'aggregation-audit', calibrate_definition_header_walrus, False, ()),
    'calibrate-sys-modules-chain-poison': (
        'aggregation-audit', calibrate_sys_modules_chain_poison, False, ()),
    'calibrate-walrus-receiver-local': (
        'aggregation-audit', calibrate_walrus_receiver_local, False, ()),
    'calibrate-dict-namespace-update': (
        'aggregation-audit', calibrate_dict_namespace_update, False, ()),
    'calibrate-dict-namespace-augassign': (
        'aggregation-audit', calibrate_dict_namespace_augassign, False, ()),
    'calibrate-dunder-setattr-receiver': (
        'aggregation-audit', calibrate_dunder_setattr_receiver, False, ()),
    'calibrate-object-setattr-unbound': (
        'aggregation-audit', calibrate_object_setattr_unbound, False, ()),
    'calibrate-dict-alias-write': (
        'aggregation-audit', calibrate_dict_alias_write, False, ()),
    'calibrate-import-call-modules': (
        'aggregation-audit', calibrate_import_call_modules, False, ()),
    'calibrate-dict-update-unbound': (
        'aggregation-audit', calibrate_dict_update_unbound, False, ()),
    'calibrate-dict-setdefault-unbound': (
        'aggregation-audit', calibrate_dict_setdefault_unbound, False, ()),
    'calibrate-dict-pop-unbound': (
        'aggregation-audit', calibrate_dict_pop_unbound, False, ()),
    'calibrate-dict-setitem-unbound': (
        'aggregation-audit', calibrate_dict_setitem_unbound, False, ()),
    'calibrate-helper-setattr': (
        'aggregation-audit', calibrate_helper_setattr, False, ()),
    'calibrate-operator-setitem': (
        'aggregation-audit', calibrate_operator_setitem, False, ()),
    'calibrate-getattr-dunder-write': (
        'aggregation-audit', calibrate_getattr_dunder_write, False, ()),
    'calibrate-getattr-builtin-write': (
        'aggregation-audit', calibrate_getattr_builtin_write, False, ()),
    'calibrate-partial-write': (
        'aggregation-audit', calibrate_partial_write, False, ()),
    'calibrate-partial-handle': (
        'aggregation-audit', calibrate_partial_handle, False, ()),
    'calibrate-tuple-attribute-write': (
        'aggregation-audit', calibrate_tuple_attribute_write, False, ()),
    'calibrate-eval-literal-write': (
        'aggregation-audit', calibrate_eval_literal_write, False, ()),
    'calibrate-exec-receiver-dict': (
        'aggregation-audit', calibrate_exec_receiver_dict, False, ()),
    'calibrate-locals-write': (
        'aggregation-audit', calibrate_locals_write, False, ()),
    'calibrate-namespace-unbound-update': (
        'aggregation-audit', calibrate_namespace_unbound_update, False, ()),
    'calibrate-builtins-type-alias': (
        'aggregation-audit', calibrate_builtins_type_alias, False, ()),
    'calibrate-dict-alias-augassign': (
        'aggregation-audit', calibrate_dict_alias_augassign, False, ()),
    'calibrate-walrus-receiver-write': (
        'aggregation-audit', calibrate_walrus_receiver_write, False, ()),
    'calibrate-alias-chain-reverse': (
        'aggregation-audit', calibrate_alias_chain_reverse, False, ()),
    'calibrate-sm-pair-value': (
        'aggregation-audit', calibrate_sm_pair_value, False, ()),
    'calibrate-sm-loop-rebind': (
        'aggregation-audit', calibrate_sm_loop_rebind, False, ()),
    'calibrate-sm-unbound-update': (
        'aggregation-audit', calibrate_sm_unbound_update, False, ()),
    'calibrate-getattr-dict-subscript': (
        'aggregation-audit', calibrate_getattr_dict_subscript, False, ()),
    'calibrate-builtins-subscript-setattr': (
        'aggregation-audit', calibrate_builtins_subscript_setattr, False, ()),
    'calibrate-getattr-import-alias': (
        'aggregation-audit', calibrate_getattr_import_alias, False, ()),
    'calibrate-builtins-module-alias': (
        'aggregation-audit', calibrate_builtins_module_alias, False, ()),
    'calibrate-builtins-getattr-subscript': (
        'aggregation-audit', calibrate_builtins_getattr_subscript, False, ()),
    'calibrate-builtins-subscript-vars': (
        'aggregation-audit', calibrate_builtins_subscript_vars, False, ()),
    'calibrate-status-getattr-alias': (
        'aggregation-audit', calibrate_status_getattr_alias, False, ()),
    'calibrate-write-handle-name': (
        'aggregation-audit', calibrate_write_handle_name, False, ()),
    'calibrate-write-handle-getattr': (
        'aggregation-audit', calibrate_write_handle_getattr, False, ()),
    'calibrate-write-handle-attrgetter': (
        'aggregation-audit', calibrate_write_handle_attrgetter, False, ()),
    'calibrate-star-setattr': (
        'aggregation-audit', calibrate_star_setattr, False, ()),
    'calibrate-star-forward': (
        'aggregation-audit', calibrate_star_forward, False, ()),
    'calibrate-unpack-receiver': (
        'aggregation-audit', calibrate_unpack_receiver, False, ()),
    'calibrate-for-receiver': (
        'aggregation-audit', calibrate_for_receiver, False, ()),
    'calibrate-default-receiver': (
        'aggregation-audit', calibrate_default_receiver, False, ()),
    'calibrate-with-receiver': (
        'aggregation-audit', calibrate_with_receiver, False, ()),
    'calibrate-with-key-rebind': (
        'aggregation-audit', calibrate_with_key_rebind, False, ()),
    'calibrate-globals-tuple-target': (
        'aggregation-audit', calibrate_globals_tuple_target, False, ()),
    'calibrate-sm-callable-setitem': (
        'aggregation-audit', calibrate_sm_callable_setitem, False, ()),
    'calibrate-sm-mapping-augassign': (
        'aggregation-audit', calibrate_sm_mapping_augassign, False, ()),
    'calibrate-builtins-alias-subscript': (
        'aggregation-audit', calibrate_builtins_alias_subscript, False, ()),
    'calibrate-getattr-dynamic-call': (
        'aggregation-audit', calibrate_getattr_dynamic_call, False, ()),
    'calibrate-globals-alias-augassign': (
        'aggregation-audit', calibrate_globals_alias_augassign, False, ()),
    'calibrate-global-walrus-rebind': (
        'aggregation-audit', calibrate_global_walrus_rebind, False, ()),
    'calibrate-unpack-star-target': (
        'aggregation-audit', calibrate_unpack_star_target, False, ()),
    'calibrate-unpack-star-value': (
        'aggregation-audit', calibrate_unpack_star_value, False, ()),
    'calibrate-unpack-iter-for': (
        'aggregation-audit', calibrate_unpack_iter_for, False, ()),
    'calibrate-with-handle-name': (
        'aggregation-audit', calibrate_with_handle_name, False, ()),
    'calibrate-getattr-handle-name': (
        'aggregation-audit', calibrate_getattr_handle_name, False, ()),
    'calibrate-getattr-handle-dict': (
        'aggregation-audit', calibrate_getattr_handle_dict, False, ()),
    'calibrate-nonlocal-common-rebind': (
        'aggregation-audit', calibrate_nonlocal_common_rebind, False, ()),
    'calibrate-builtin-last-binding': (
        'aggregation-audit', calibrate_builtin_last_binding, False, ()),
    'calibrate-partial-prebound': (
        'aggregation-audit', calibrate_partial_prebound, False, ()),
    'calibrate-partial-name-status': (
        'aggregation-audit', calibrate_partial_name_status, False, ()),
    'calibrate-builtins-chain-four': (
        'aggregation-audit', calibrate_builtins_chain_four, False, ()),
    'calibrate-update-unregistered': (
        'aggregation-audit', calibrate_update_unregistered, False, ()),
    'calibrate-walrus-exit': (
        'aggregation-audit', calibrate_walrus_exit, False, ()),
    'calibrate-status-update-kwarg': (
        'aggregation-audit', calibrate_status_update_kwarg, False, ()),
    'calibrate-helper-bare-return': (
        'aggregation-audit', calibrate_helper_bare_return, False, ()),
    'calibrate-helper-exit-call': (
        'aggregation-audit', calibrate_helper_exit_call, False, ()),
    'calibrate-augassign-unregistered': (
        'aggregation-audit', calibrate_augassign_unregistered, False, ()),
    'calibrate-clean-argument': (
        'batch-aggregation',
        lambda rows: match(rows, 'batch-aggregation', {
            'caseFailure.status': 'PASS',
            'caseFailure.returncode': 0,
        }),
        False,
        ()),
    'restored-skip-accepted': (
        'batch-aggregation',
        lambda rows: match(rows, 'batch-aggregation', {
            'negativeRestored.skipStatus': 'PASS',
            'negativeRestored.skipReturncode': 0,
        }),
        False,
        ()),
    # #892 fix-r34: see the MUTATIONS note above.
    'calibrate-closure-default': (
        'aggregation-audit', calibrate_closure_default, False, ()),
    'calibrate-global-handoff': (
        'aggregation-audit', calibrate_global_handoff, False, ()),
    'calibrate-star-from-name': (
        'aggregation-audit', calibrate_star_from_name, False, ()),
    'calibrate-star-target-rest': (
        'aggregation-audit', calibrate_star_target_rest, False, ()),
    'calibrate-handle-annassign': (
        'aggregation-audit', calibrate_handle_annassign, False, ()),
    'calibrate-handle-argument': (
        'aggregation-audit', calibrate_handle_argument, False, ()),
    'calibrate-ordered-binding': (
        'aggregation-audit', calibrate_ordered_binding, False, ()),
    'calibrate-partial-update-prebound': (
        'aggregation-audit', calibrate_partial_update_prebound, False, ()),
    'calibrate-partial-setdefault-prebound': (
        'aggregation-audit', calibrate_partial_setdefault_prebound, False, ()),
    'calibrate-builtins-chain-fiftyone': (
        'aggregation-audit', calibrate_builtins_chain_fiftyone, False, ()),
    # #1953 fix-r35: see the MUTATIONS note above.
    'calibrate-loop-backedge-alias': (
        'aggregation-audit', calibrate_loop_backedge_alias, False, ()),
    'calibrate-sameline-restore': (
        'aggregation-audit', calibrate_sameline_restore, False, ()),
    'calibrate-shared-seen-paths': (
        'aggregation-audit', calibrate_shared_seen_paths, False, ()),
    'calibrate-decorator-lambda-scope': (
        'aggregation-audit', calibrate_decorator_lambda_scope, False, ()),
    'calibrate-nonlocal-walrus-withdraw': (
        'aggregation-audit', calibrate_nonlocal_walrus_withdraw, False, ()),
    'calibrate-handle-list-literal': (
        'aggregation-audit', calibrate_handle_list_literal, False, ()),
    'calibrate-handle-dict-literal': (
        'aggregation-audit', calibrate_handle_dict_literal, False, ()),
    'calibrate-handle-yield': (
        'aggregation-audit', calibrate_handle_yield, False, ()),
    'calibrate-getattribute-handover': (
        'aggregation-audit', calibrate_getattribute_handover, False, ()),
    'calibrate-handle-dictcomp-escape': (
        'aggregation-audit', calibrate_handle_dictcomp_escape, False, ()),
    'calibrate-handle-listcomp-escape': (
        'aggregation-audit', calibrate_handle_listcomp_escape, False, ()),
    'calibrate-handle-aliased-escape': (
        'aggregation-audit', calibrate_handle_aliased_escape, False, ()),
    'calibrate-getattribute-write-attribute': (
        'aggregation-audit', calibrate_getattribute_write_attribute, False, ()),
    'calibrate-getattr-const-subscript-write': (
        'aggregation-audit', calibrate_getattr_const_subscript_write, False, ()),
    'calibrate-match-capture-helper': (
        'aggregation-audit', calibrate_match_capture_helper, False, ()),
    'calibrate-comprehension-write-helper': (
        'aggregation-audit', calibrate_comprehension_write_helper, False, ()),
    'calibrate-mixed-alias-binding': (
        'aggregation-audit', calibrate_mixed_alias_binding, False, ()),
    'calibrate-handle-container-alias': (
        'aggregation-audit', calibrate_handle_container_alias, False, ()),
    'calibrate-handle-mapping-write': (
        'aggregation-audit', calibrate_handle_mapping_write, False, ()),
    'calibrate-list-const-subscript-write': (
        'aggregation-audit', calibrate_list_const_subscript_write, False, ()),
    'calibrate-comprehension-filter-write': (
        'aggregation-audit', calibrate_comprehension_filter_write, False, ()),
    'calibrate-match-capture-receiver': (
        'aggregation-audit', calibrate_match_capture_receiver, False, ()),
    'calibrate-match-capture-setattr-receiver': (
        'aggregation-audit', calibrate_match_capture_setattr_receiver, False, ()),
    'calibrate-getattribute-attr-spelling': (
        'aggregation-audit', calibrate_getattribute_attr_spelling, False, ()),
    'calibrate-getattribute-name-spelling': (
        'aggregation-audit', calibrate_getattribute_name_spelling, False, ()),
    'calibrate-for-wrapped-container': (
        'aggregation-audit', calibrate_for_wrapped_container, False, ()),
    'calibrate-for-target-after-loop': (
        'aggregation-audit', calibrate_for_target_after_loop, False, ()),
    'calibrate-for-slice': (
        'aggregation-audit', calibrate_for_slice, False, ()),
    'calibrate-for-copy': (
        'aggregation-audit', calibrate_for_copy, False, ()),
    'calibrate-comp-result-receiver': (
        'aggregation-audit', calibrate_comp_result_receiver, False, ()),
    'calibrate-for-target-rebind-if': (
        'aggregation-audit', calibrate_for_target_rebind_if, False, ()),
    'calibrate-for-target-after-match': (
        'aggregation-audit', calibrate_for_target_after_match, False, ()),
    'calibrate-comp-dict-result': (
        'aggregation-audit', calibrate_comp_dict_result, False, ()),
    'calibrate-comp-name-iter': (
        'aggregation-audit', calibrate_comp_name_iter, False, ()),
    'calibrate-for-target-return': (
        'aggregation-audit', calibrate_for_target_return, False, ()),
    'calibrate-for-target-alias': (
        'aggregation-audit', calibrate_for_target_alias, False, ()),
    'calibrate-starred-const-subscript-write': (
        'aggregation-audit', calibrate_starred_const_subscript, False, ()),
    'calibrate-comp-handle-relay': (
        'aggregation-audit', calibrate_comp_handle_relay, False, ()),
    'calibrate-for-target-finally-rebind': (
        'aggregation-audit', calibrate_for_target_finally_rebind, False, ()),
    'calibrate-for-target-closure': (
        'aggregation-audit', calibrate_for_target_closure, False, ()),
    'calibrate-for-target-shadowed-fresh': (
        'aggregation-audit', calibrate_for_target_shadowed_fresh, False, ()),
    'calibrate-for-target-with-alias': (
        'aggregation-audit', calibrate_for_target_with_alias, False, ()),
    'calibrate-for-target-except-alias': (
        'aggregation-audit', calibrate_for_target_except_alias, False, ()),
    'calibrate-for-target-name-iter': (
        'aggregation-audit', calibrate_for_target_name_iter, False, ()),
    'calibrate-comp-handle-relay-for': (
        'aggregation-audit', calibrate_comp_handle_relay_for, False, ()),
    'calibrate-comp-handle-relay-chain': (
        'aggregation-audit', calibrate_comp_handle_relay_chain, False, ()),
    'calibrate-for-target-assign-call': (
        'aggregation-audit', calibrate_for_target_assign_call, False, ()),
    'calibrate-for-target-def-default': (
        'aggregation-audit', calibrate_for_target_def_default, False, ()),
    'calibrate-for-target-readonly-call-leak': (
        'aggregation-audit', calibrate_for_target_readonly_call_leak, False, ()),
    'calibrate-for-target-nested-param': (
        'aggregation-audit', calibrate_for_target_nested_param, False, ()),
    'calibrate-for-target-compare-call-leak': (
        'aggregation-audit', calibrate_for_target_compare_call_leak, False, ()),
    'calibrate-for-target-outer-shadow-fresh': (
        'aggregation-audit', calibrate_for_target_outer_shadow_fresh, False, ()),
    'calibrate-for-target-outer-shadow-print': (
        'aggregation-audit', calibrate_for_target_outer_shadow_print, False, ()),
    'calibrate-for-target-ambiguous-literal': (
        'aggregation-audit', calibrate_for_target_ambiguous_literal, False, ()),
    'calibrate-for-target-comp-element-call': (
        'aggregation-audit', calibrate_for_target_comp_element_call, False, ()),
    'calibrate-for-target-call-attribute': (
        'aggregation-audit', calibrate_for_target_call_attribute, False, ()),
    'calibrate-for-target-call-attribute-alias': (
        'aggregation-audit', calibrate_for_target_call_attribute_alias, False, ()),
    'calibrate-for-target-false-filter-write': (
        'aggregation-audit', calibrate_for_target_false_filter_write, False, ()),
    'calibrate-for-target-class-body-clear': (
        'aggregation-audit', calibrate_for_target_class_body_clear, False, ()),
    'calibrate-for-target-with-suppress': (
        'aggregation-audit', calibrate_for_target_with_suppress, False, ()),
    'calibrate-for-target-class-body-shadow': (
        'aggregation-audit', calibrate_for_target_class_body_shadow, False, ()),
    'calibrate-for-target-lambda-default': (
        'aggregation-audit', calibrate_for_target_lambda_default, False, ()),
    'calibrate-for-target-class-body-def': (
        'aggregation-audit', calibrate_for_target_class_body_def, False, ()),
    'calibrate-for-target-method-self': (
        'aggregation-audit', calibrate_for_target_method_self, False, ()),
    'calibrate-for-target-conditional-literal-rebind': (
        'aggregation-audit', calibrate_for_target_conditional_literal_rebind,
        False, ()),
    'calibrate-for-target-enclosing-binding-after-use': (
        'aggregation-audit',
        calibrate_for_target_enclosing_binding_after_use, False, ()),
    'calibrate-for-target-tuple-partial-alias': (
        'aggregation-audit', calibrate_for_target_tuple_partial_alias,
        False, ()),
    'calibrate-for-target-comp-partial-shadow': (
        'aggregation-audit', calibrate_for_target_comp_partial_shadow,
        False, ()),
    'calibrate-for-target-assert-msg': (
        'aggregation-audit', calibrate_for_target_assert_msg, False, ()),
    'calibrate-for-target-enclosing-call-between-rebinds': (
        'aggregation-audit',
        calibrate_for_target_enclosing_call_between_rebinds, False, ()),
    'calibrate-for-target-class-body-compound': (
        'aggregation-audit', calibrate_for_target_class_body_compound,
        False, ()),
    'calibrate-for-target-class-method-closure': (
        'aggregation-audit', calibrate_for_target_class_method_closure,
        False, ()),
    'calibrate-for-target-class-body-shadowed-constructor': (
        'aggregation-audit',
        calibrate_for_target_class_body_shadowed_constructor, False, ()),
    'calibrate-for-target-module-import-alias': (
        'aggregation-audit', calibrate_for_target_module_import_alias, False, ()),
    'calibrate-for-target-class-body-copy-alias': (
        'aggregation-audit', calibrate_for_target_class_body_copy_alias, False, ()),
    'calibrate-for-target-nested-class-method': (
        'aggregation-audit', calibrate_for_target_nested_class_method, False, ()),
    'calibrate-for-target-conditional-rebind': (
        'aggregation-audit', calibrate_for_target_conditional_rebind, False, ()),
    'calibrate-for-target-loop-back-edge': (
        'aggregation-audit', calibrate_for_target_loop_back_edge, False, ()),
    'calibrate-for-target-same-line-rebind': (
        'aggregation-audit', calibrate_for_target_same_line_rebind, False, ()),
    'calibrate-for-target-class-block-alias': (
        'aggregation-audit', calibrate_for_target_class_block_alias, False, ()),
    'calibrate-for-target-nested-copy-alias': (
        'aggregation-audit', calibrate_for_target_nested_copy_alias, False, ()),
    'calibrate-for-target-module-call-before-rebind': (
        'aggregation-audit', calibrate_for_target_module_call_before_rebind,
        False, ()),
    'calibrate-for-target-module-self-assignment': (
        'aggregation-audit', calibrate_for_target_module_self_assignment,
        False, ()),
    'calibrate-for-target-module-loop-first-use': (
        'aggregation-audit', calibrate_for_target_module_loop_first_use,
        False, ()),
    'calibrate-for-target-duplicated-unpack-target': (
        'aggregation-audit', calibrate_for_target_duplicated_unpack_target,
        False, ()),
    'calibrate-for-target-module-call-site-loop-edge': (
        'aggregation-audit', calibrate_for_target_module_call_site_loop_edge,
        False, ()),
    'calibrate-for-target-default-value-call': (
        'aggregation-audit', calibrate_for_target_default_value_call,
        False, ()),
    'calibrate-for-target-assignment-back-edge': (
        'aggregation-audit', calibrate_for_target_assignment_back_edge,
        False, ()),
    'calibrate-for-target-handle-conditional-overwrite': (
        'aggregation-audit', calibrate_for_target_handle_conditional_overwrite,
        False, ()),
    'calibrate-for-target-scoped-handle-receiver': (
        'aggregation-audit', calibrate_for_target_scoped_handle_receiver,
        False, ()),
    'calibrate-for-target-shadowed-constructor': (
        'aggregation-audit', calibrate_for_target_shadowed_constructor,
        False, ()),
    'calibrate-for-target-handle-container-copy': (
        'aggregation-audit', calibrate_for_target_handle_container_copy,
        False, ()),
    'calibrate-status-bound-handle-alias-chain': (
        'aggregation-audit', calibrate_status_bound_handle_alias_chain,
        False, ()),
    'calibrate-status-three-arg-handle-live': (
        'aggregation-audit', calibrate_status_three_arg_handle_live,
        False, ()),
    'calibrate-status-getattr-alias-callee': (
        'aggregation-audit', calibrate_status_getattr_alias_callee,
        False, ()),
    'calibrate-for-target-nested-container-copy': (
        'aggregation-audit', calibrate_for_target_nested_container_copy,
        False, ()),
    'calibrate-status-alias-copied-before-overwrite': (
        'aggregation-audit', calibrate_status_alias_copied_before_overwrite,
        False, ()),
    'calibrate-for-target-container-filled-after-rebind': (
        'aggregation-audit', calibrate_for_target_container_filled_after_rebind,
        False, ()),
    'calibrate-status-imported-getattr-factory': (
        'aggregation-audit', calibrate_status_imported_getattr_factory,
        False, ()),
    'calibrate-for-target-alias-of-container-copy': (
        'aggregation-audit', calibrate_for_target_alias_of_container_copy,
        False, ()),
    'calibrate-status-builtins-module-alias': (
        'aggregation-audit', calibrate_status_builtins_module_alias,
        False, ()),
    'calibrate-status-factory-import-after-assignment': (
        'aggregation-audit', calibrate_status_factory_import_after_assignment,
        False, ()),
    'calibrate-status-factory-import-conditional-overwrite': (
        'aggregation-audit',
        calibrate_status_factory_import_conditional_overwrite,
        False, ()),
    'calibrate-status-builtins-alias-self-assign': (
        'aggregation-audit',
        calibrate_status_builtins_alias_self_assign, False, ()),
    'calibrate-status-builtins-subscript-copy': (
        'aggregation-audit',
        calibrate_status_builtins_subscript_copy, False, ()),
}
if set(JUDGES) != {row[0] for row in MUTATIONS}:
    raise SystemExit('JUDGES must name exactly the MUTATIONS rows')

# #892 fix-r18: the restore window must defer the fixture's own SIGTERM exit
# instead of unwinding mid-restore (MiniMax concurrency r17 P1-1).  The raise
# lands on an arbitrary bytecode boundary; landing inside the write-back loop
# leaves a mutant in the tree and the next run reads it as a real kill.  The
# deferral protocol is deterministic and checked here on every import; the
# boundary itself is not reproducible by construction (recorded in the PR).
if defer_signal_exit(15) is not False:
    raise SystemExit('defer_signal_exit must not defer outside restore_critical')
with restore_critical():
    if defer_signal_exit(15) is not True:
        raise SystemExit('defer_signal_exit must defer inside restore_critical')
    if take_deferred_signal() != 15:
        raise SystemExit('take_deferred_signal must hand back the deferred signum')
    if take_deferred_signal() is not None:
        raise SystemExit('take_deferred_signal must consume the deferred signum once')
if defer_signal_exit(15) is not False:
    raise SystemExit('defer_signal_exit must not defer after the window closes')


def pgrep_launcher():
    try:
        process = subprocess.run(
            ['pgrep', '-fl', 'session-domain-[c]leanup/launcher.py'],
            capture_output=True, text=True, timeout=5)
    except (OSError, subprocess.SubprocessError) as error:
        return f'BLOCKED: {type(error).__name__}: {error}\n'
    return f'rc={process.returncode}\n{process.stdout}{process.stderr}'


def read_summary(case):
    path = case / 'summary.json'
    if not path.is_file():
        return None, 'summary.json missing'
    try:
        return json.loads(path.read_text()), None
    except (OSError, ValueError) as error:
        return None, f'summary.json unreadable: {type(error).__name__}: {error}'


# The batch holds the host-wide parity lease (``mutating_sources`` takes it for
# any ``scripts/gateway-parity`` source) from the first mutation to the restored
# positive.  Before ``fix-r9`` that hold had no bound at all: a 78-mutant batch
# at the per-check ceiling is 78 * 180 s, far past any deadline a concurrent
# suite sets for itself, so a parity round could wait without any bound
# (MiniMax concurrency r7 P1-1).  The budget below bounds the *whole* hold,
# restored positive and the kill grace included.  It does not remove the
# pre-existing race with a suite that is already running: such a suite asks for
# the lease with its own 30 s deadline and still times out while the batch holds
# it.  That needs the lease to shrink to the source-rewrite window -- a shared
# ``negative_process.mutating_sources`` change, tracked in #1719.
#
# The bound is *not* a stale-lease backstop: ``common.parity_lock`` publishes
# ``cleanupPending`` when it takes the lease and refuses to take over an owner
# that carries the flag (``scripts/gateway-parity/common.py`` takeover guard;
# ``scripts/gateway-parity/README.md`` "dead_pid/expired" section), so a
# SIGKILLed batch leaves a lease only the coordinator can remove by hand.  What
# the budget buys is that this apparatus is not the one that creates that
# residue, and that concurrent suites never wait past one bounded hold.
# Measured full batch: 160 s.
LEASE_BUDGET_SECONDS = 900
CHECK_TIMEOUT_SECONDS = 180
# Imported rather than restated: the reserve below is only correct while this
# number equals the upstream kill grace, and a copy drifts silently (MiniMax
# boundary r14 P2-2 / simplify r14 P2-4).
KILL_GRACE_SECONDS = KILL_TIMEOUT
# Per-check overhead the reserve has to absorb besides the timeout: the kill
# grace is charged twice by ``negative_process._collect`` (``communicate`` then
# ``wait``), so one check can hold the lease for ``timeout + 2 * grace``; the
# restore/digest of the sources is charged once more on top (MiniMax concurrency
# r10 P2-2 / r11 P2-4).
TAIL_OVERHEAD_SECONDS = 20


def lease_budget_left(started, now):
    """Seconds the mutation loop may still spend before the reserved tail.

    Two checks' worth stay reserved: the one that is about to start and the
    restored positive that always runs at the end.  Without that reserve the
    last mutation could start at the edge of the budget and push the hold past
    ``LEASE_BUDGET_SECONDS`` (GLM boundary r9 P1-1).  Each reserved check is
    charged at its real worst case, ``timeout + 2 * grace`` (MiniMax boundary
    r11 P2-4).
    """
    reserve = 2 * (CHECK_TIMEOUT_SECONDS + 2 * KILL_GRACE_SECONDS) + TAIL_OVERHEAD_SECONDS
    return LEASE_BUDGET_SECONDS - (now - started) - reserve


def terminate_cleanly(signum, _frame):
    """Unwind on SIGTERM/SIGHUP so the finally blocks restore the sources.

    The default handler kills the interpreter on the spot: the mutated frozen
    source stays in the tree and the host-wide parity lease is never released,
    so the next suite waits out its own deadline and the tree is left dirty
    (MiniMax concurrency r10 P1-1).  SIGKILL cannot be caught, and the residue
    it leaves is *not* aged out: ``common.parity_lock`` marks the lease
    ``cleanupPending`` and refuses to take over such an owner, so the lock has
    to be removed by hand (``scripts/gateway-parity/README.md``); #1719 tracks
    the recovery steps (MiniMax concurrency r11 P1-1 / GLM concurrency r11 P1-1).

    The raise lands wherever the interpreter happens to be, including inside
    the write-back loop of ``negative_process.mutating_sources``: an exit there
    leaves a mutant in the tree and the next run reads it as a real kill
    (MiniMax concurrency r17 P1-1).  While that restore is in progress the
    exit is therefore recorded and re-raised by ``mutating_sources`` once the
    sources and the lease are settled; the process still ends with 128+signum.
    """
    if defer_signal_exit(signum):
        return
    raise SystemExit(128 + signum)


def _install_terminate_handlers():
    """Install the catch-and-unwind handler for every signal we can (#892 r23).

    ``_SHIELDED_SIGNALS`` defers SIGQUIT inside the restore window, but the
    batch loop registered only SIGTERM/SIGHUP; outside the window a ``^\\``
    still hit the default kill-with-core disposition, so the finally blocks
    never ran and a mutant stayed in the tree -- exactly the SIGTERM hazard
    ``terminate_cleanly`` exists for (anthropic sibling r22 P1 / GLM boundary
    r22 P2, taken at the higher grade).  Split out of ``main`` so a test can
    install the very handlers the harness runs with.
    """
    signal.signal(signal.SIGTERM, terminate_cleanly)
    if hasattr(signal, 'SIGHUP'):
        signal.signal(signal.SIGHUP, terminate_cleanly)
    if hasattr(signal, 'SIGQUIT'):
        signal.signal(signal.SIGQUIT, terminate_cleanly)


def run_check(case, mode, timeout=CHECK_TIMEOUT_SECONDS):
    return run_owned([sys.executable, '-I', str(CHECK), '--output', str(case),
                      '--mode', mode], cwd=ROOT, env=dict(os.environ), timeout=timeout)


def classify(name, judge_spec, result, case):
    """KILLED only for the mutation's own expected failure fields.

    A probe/environment error that the mutation did not declare is INVALID:
    otherwise an unmutated implementation whose group signal is denied can
    satisfy the expected fields and be mis-counted as a kill.
    """
    mode, judge, uses_launcher, allowed_errors = judge_spec
    if isinstance(result, subprocess.TimeoutExpired):
        return 'INVALID', f'check timed out: {result}'
    if isinstance(result, BaseException):
        return 'INVALID', f'check failed to start/finish: {type(result).__name__}: {result}'
    summary, error = read_summary(case)
    if summary is None:
        return 'INVALID', error
    rows = summary.get('results') or []
    if uses_launcher:
        launcher = [row for row in rows if row.get('mode') == mode]
        if not launcher:
            return 'INVALID', f'no launcher result for mode {mode}'
        if launcher[0].get('launcherExitCode') != 0:
            return 'INVALID', (f"launcher startup failed: exit={launcher[0].get('launcherExitCode')!r} "
                               f"status={launcher[0].get('launcherStatus')!r}")
        probe = launcher[0].get('probe') or {}
        for field in ('signalError', 'probeError'):
            value = probe.get(field)
            if value and field not in allowed_errors:
                return 'INVALID', f'undeclared probe environment error {field}={value!r}'
    try:
        ok, detail = judge(rows)
    except MissingKernelBranchEvidence as error:
        return 'INVALID', str(error)
    if ok:
        if result.returncode == 0:
            return 'INVALID', 'expected result present but the check exited 0'
        return 'KILLED', detail
    if summary.get('status') != 'FAIL':
        return 'SURVIVED', f'check reported {summary.get("status")!r}; {detail}'
    return 'SURVIVED', f'check failed without this mutation\'s fields: {detail}'


def load_pristine_common():
    spec = importlib.util.spec_from_file_location('session_cleanup_common', COMMON_SOURCE)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def require_single_anchor(original, old, name):
    if original.count(old) != 1:
        raise ValueError(f'#{name} anchor expired: count={original.count(old)}')


def require_restored(source, digest):
    if hashlib.sha256(source.read_bytes()).hexdigest() != digest:
        raise RuntimeError(f'source restore hash mismatch: {source.name}')


DARWIN_ONLY = frozenset(('empty-group-probe-removed','sigterm-removed',
    'sigkill-escalation-removed','eperm-census-ignored','blocked-branch-removed'))
LINUX_ONLY = frozenset(('kernel-source-lease-cleanup-pending-guard-removed','kernel-source-lease-token-guard-removed','kernel-source-lease-timestamp-guard-removed','kernel-source-lease-flock-guard-removed','kernel-source-lease-gate-removed','linux-initial-identity-gate-removed','linux-pre-reap-identity-gate-removed','linux-refusal-erases-finding','linux-empty-group-probe-removed','linux-sigterm-removed',
    'linux-sigkill-escalation-removed','linux-positive-finding-dropped-on-denial',
    'linux-unknown-error-means-empty'))


EXPECTED_MUTATION_ROSTERS = {'declared': {'count': 349, 'sha256': '301feec75d6bb2dd72cb6f9cd4680049b4cd0b7fd94194bc7355990f51f45688'}, 'linux': {'count': 344, 'sha256': '588aa695518e9706cc572b65c6b3dc2c209dda063859e15941bd610d76573e8d'}, 'darwin': {'count': 336, 'sha256': '3230002e56264111754c5c8b560c36696bf900f5f52a14bf8dadfafdab19f1e5'}, 'other': {'count': 331, 'sha256': 'aab79d771a3c58005dda6863b2f799bd8d2417def547d48d9c2808dcbd38ce2d'}}


def applicable_mutations(platform):
    return [row for row in MUTATIONS
            if (row[0] not in DARWIN_ONLY or platform=='darwin')
            and (row[0] not in LINUX_ONLY or platform=='linux')]


def mutation_inventory_matches(platform, eligible):
    def stamp(rows):
        names=[row[0]for row in rows]
        return {'count':len(names),'sha256':hashlib.sha256(
            json.dumps(names,separators=(',',':'),ensure_ascii=True).encode()).hexdigest()}
    key=platform if platform in ('linux','darwin') else 'other'
    return (bool(eligible) and stamp(MUTATIONS)==EXPECTED_MUTATION_ROSTERS['declared']
            and stamp(eligible)==EXPECTED_MUTATION_ROSTERS[key])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--baseline', default='HEAD',
                        choices=('HEAD',), help='sources must equal committed HEAD')
    parser.add_argument('--only', action='append', default=[],
                        help='mutant name; repeat to select more (still runs the restored positive)')
    args = parser.parse_args()
    # Spelled out one by one: the anchor selfcheck rejects ``getattr`` in a
    # file that takes a snapshot, because a dynamic attribute lookup could also
    # reach a mutating capability.
    _install_terminate_handlers()
    eligible = applicable_mutations(sys.platform)
    known = {row[0] for row in eligible}
    if not mutation_inventory_matches(sys.platform,eligible):
        common=committed_status_contract(ROOT)
        return common.aggregate_status('BLOCKED')[1]
    if set(args.only) - known or len(args.only) != len(set(args.only)):
        common = committed_status_contract(ROOT)
        return common.aggregate_status('BLOCKED')[1]
    selected = ([row for row in eligible if row[0] in args.only]
                if args.only else list(eligible))
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    (output / 'pgrep-before.txt').write_text(pgrep_launcher())
    summary = []
    invalid = []
    survived = []
    sources = sorted({COMMON_SOURCE, *(Path(row[1]) for row in MUTATIONS)}, key=str)
    try:
        with mutating_sources(ROOT, sources, baseline=args.baseline,
                              lock_timeout=LEASE_BUDGET_SECONDS) as originals:
            # Import only after every source, including the verdict table, is pristine.
            common = load_pristine_common()
            # The budget has to stay below the age at which a *different*
            # apparatus gives up on this lease, so a drift in either constant
            # would silently break the promise; the two are checked against each
            # other here.  An explicit raise, not an ``assert``: ``python -O``
            # would strip the guard and leave only the comment (GLM concurrency
            # r11 P3-4 / MiniMax boundary r11 P3-5).
            if not (LEASE_BUDGET_SECONDS + TAIL_OVERHEAD_SECONDS < common.MAX_AGE):
                print(f'negative-controls: LEASE_BUDGET_SECONDS '
                      f'{LEASE_BUDGET_SECONDS}s + overhead {TAIL_OVERHEAD_SECONDS}s '
                      f'must stay below common.MAX_AGE {common.MAX_AGE}s',
                      file=sys.stderr)
                raise SystemExit(common.aggregate_status('BLOCKED')[1])
            # INVALID remains BLOCKED, never a killed mutant or a passing batch.
            MUTATION_STATUS = common.MUTATION_STATUS
            digests = {source: hashlib.sha256(originals[source]).hexdigest()
                       for source in sources}
            contents = {source: originals[source].decode() for source in sources}
            started = time.monotonic()
            try:
                for mutation in selected:
                    name, source, old, new = mutation
                    source = Path(source)
                    judge_spec = JUDGES[name]
                    mode = judge_spec[0]
                    case = output / name  # check_cleanup.py owns/creates this directory
                    if lease_budget_left(started, time.monotonic()) <= 0:
                        # The lease is host-wide: stop before the batch can
                        # outlive it, and never count an unrun mutation as a
                        # kill.  INVALID aggregates to BLOCKED, never to PASS.
                        entry = {'mutation': name, 'mode': mode, 'verdict': 'INVALID',
                                 'detail': (f'batch spent the {LEASE_BUDGET_SECONDS}s parity '
                                            'lease budget before this mutation (two checks '
                                            'stay reserved for the tail); re-run the '
                                            'remainder with --only'),
                                 'returncode': None,
                                 'source': Path(source).relative_to(ROOT).as_posix()}
                        summary.append(entry)
                        invalid.append(entry)
                        print(f'{name}: INVALID {entry["detail"]}', flush=True)
                        continue
                    returncode = None
                    try:
                        require_single_anchor(contents[source], old, name)
                        changed = contents[source].replace(old, new, 1)
                        compile(changed, str(source), 'exec')
                        (output / f'{name}-compile.log').write_text('Python compile: PASS\n')
                        source.write_text(changed)
                        try:
                            run = run_check(case, mode)
                            returncode = run.returncode
                            log = run.stdout + run.stderr
                            (output / f'{name}.log').write_text(log)
                            verdict, detail = classify(name, judge_spec, run, case)
                        except (subprocess.SubprocessError, OSError) as error:
                            (output / f'{name}.log').write_text(f'{type(error).__name__}: {error}\n')
                            verdict, detail = classify(name, judge_spec, error, case)
                    except Exception as error:
                        verdict, detail = 'INVALID', f'{type(error).__name__}: {error}'
                    finally:
                        source.write_bytes(originals[source])
                        require_restored(source, digests[source])
                    entry = {'mutation': name, 'mode': mode, 'verdict': verdict,
                             'detail': detail, 'returncode': returncode,
                             'source': source.relative_to(ROOT).as_posix()}
                    summary.append(entry)
                    if verdict == 'INVALID':
                        invalid.append(entry)
                    elif verdict == 'SURVIVED':
                        survived.append(entry)
                    print(f'{name}: {verdict} {detail}'.rstrip(), flush=True)
            finally:
                for source in sources:
                    source.write_bytes(originals[source])
            restored_case = output / 'restored-positive'  # must not pre-exist
            restored_error = None
            try:
                restored = run_check(restored_case, 'all')
                (output / 'restored-positive.log').write_text(restored.stdout + restored.stderr)
                restored_summary, restored_error = read_summary(restored_case)
            except (subprocess.SubprocessError, OSError) as error:
                restored = None
                restored_summary = None
                restored_error = f'{type(error).__name__}: {error}'
            expected_status = 'PASS' if sys.platform in ('darwin', 'linux') else 'PASS_WITH_SKIPS'
            expected_code = 0 if expected_status == 'PASS' else 1
            restored_ok = False
            if restored_summary is not None:
                rows = restored_summary.get('results') or []
                wiring = [row for row in rows if row.get('mode') == 'wiring']
                if expected_status == 'PASS':
                    rows_ok = all(row.get('passed') is True for row in rows)
                else:
                    # Same strict predicate as the Darwin arm: ``in (True, None)``
                    # would let ``passed: 1`` through (``1 == True``), which the
                    # fixture's own summarize_results rejects (GLM boundary r20
                    # P2-2).
                    rows_ok = (all(row.get('passed') is True
                                   or row.get('passed') is None for row in rows)
                               and any(row.get('passed') is None for row in rows))
                restored_ok = (restored.returncode == expected_code
                               and restored_summary.get('status') == expected_status
                               and bool(rows) and rows_ok
                               and bool(wiring) and wiring[0].get('status')
                               == 'PASS_WITH_CLEANUP_UNVERIFIED')
                if not restored_ok:
                    restored_error = (f'rc={restored.returncode} status={restored_summary.get("status")!r} '
                                      f'rows={len(rows)} wiring={bool(wiring)}')
            (output / 'pgrep-after.txt').write_text(pgrep_launcher())
            # The restored positive is a child of the same batch: a PASS_WITH_SKIPS
            # child (unsupported-platform skips) can never become PASS/0.
            top_status, exit_code = common.aggregate_status(
                *(MUTATION_STATUS.get(entry['verdict'], 'FAIL') for entry in summary),
                restored_summary.get('status') if restored_ok else 'FAIL')
            report = {'status': top_status, 'sourceSha256': digests[SOURCE],
                      'sourceSha256All': {source.relative_to(ROOT).as_posix(): digests[source]
                                          for source in sources},
                      'baseline': args.baseline,
                      'selected': [row[0] for row in selected],
                      'declaredCount':len(MUTATIONS),'applicableCount':len(eligible),
                      'notApplicable':[row[0]for row in MUTATIONS if row not in eligible],
                      'hostPlatform':sys.platform,
                      'mutations': summary, 'restoredPass': restored_ok,
                      'restoredStatus': (restored_summary.get('status')
                                         if restored_summary is not None else None),
                      'restoredError': restored_error,
                      'invalid': [row['mutation'] for row in invalid],
                      'survived': [row['mutation'] for row in survived]}
            (output / 'summary.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
            restored_label = (restored_summary.get('status')
                              if restored_summary is not None else 'FAIL')
            print(f'restored positive: {restored_label} '
                  f'{"ok" if restored_ok else "FAIL"} {restored_error or ""}'.rstrip(),
                  flush=True)
            print(json.dumps({'status': top_status, 'exitCode': exit_code,
                              'killed': len(summary) - len(invalid) - len(survived),
                              'invalid': len(invalid), 'survived': len(survived),
                              'restoredPass': restored_ok}, ensure_ascii=False))
            return exit_code
    except Exception as error:
        common = committed_status_contract(ROOT)
        report = {'status': 'BLOCKED', 'restoredPass': False,
                  'mutations': [{'mutation': row[0], 'verdict': 'INVALID',
                                 'detail': f'source guard: {error}'} for row in selected]}
        (output / 'summary.json').write_text(json.dumps(report, indent=2) + '\n')
        return common.aggregate_status('BLOCKED')[1]
    return common.aggregate_status('BLOCKED')[1]


if __name__ == '__main__':
    raise SystemExit(main())

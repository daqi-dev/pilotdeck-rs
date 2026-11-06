#!/usr/bin/env python3
"""Post-commit source negative controls for #164.

This runner mutates the tracked apparatus source in place and requires the
behavior check to turn red for the specific mutation, not for an unrelated
error.  It must run only after the #164 changes are committed:
``mutating_sources`` proves the named file is byte-identical to its baseline
before writing anything, and refuses otherwise.  The new check/launcher files
beside this script are not mutated, so their pre-commit absence from HEAD does
not matter.

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
from negative_process import mutating_sources, run_owned, wait_for_slot
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
CHECK = Path(__file__).resolve().with_name('check_cleanup.py')
NEGATIVE = Path(__file__).resolve()

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


def calibrate_param_augassign_opaque(rows):
    """#892 r13: ``report |= payload`` on a parameter receiver is the opaque
    form of the r12 literal fix (MiniMax sibling r12 P1-3)."""
    ok, detail = _unregistered_publish(rows)
    if not ok:
        return ok, detail
    return True, ('aggregation-audit: parameter-receiver opaque |= enumeration '
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
    ok, reason = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, reason
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    failures = row.get('failures') or []
    if not any('unregistered aggregation point' in failure
               and failure.rstrip().endswith(":publish markers=['write-status']")
               for failure in failures):
        return False, (f'failures={failures!r}, want the unregistered publish '
                       "point with markers=['write-status']")
    return True, 'aggregation-audit: augmented |= enumeration flagged, passed=False'


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
    ok, detail = _unregistered_publish(rows)
    if not ok:
        return ok, detail
    return True, ('aggregation-audit: parameter-receiver |= enumeration '
                  'flagged, passed=False')


def calibrate_param_update(rows):
    """#892 r12: ``report.update(payload)`` on a parameter receiver, same
    enumeration hole as the ``|=`` spelling (GLM boundary r11 P1-1)."""
    ok, detail = _unregistered_publish(rows)
    if not ok:
        return ok, detail
    return True, ('aggregation-audit: parameter-receiver update enumeration '
                  'flagged, passed=False')


def _unregistered_publish(rows):
    ok, reason = match(rows, 'aggregation-audit', {'passed': False})
    if not ok:
        return ok, reason
    row, error = _audit_row(rows)
    if row is None:
        return False, error
    failures = row.get('failures') or []
    if not any('unregistered aggregation point' in failure
               and failure.rstrip().endswith(":publish markers=['write-status']")
               for failure in failures):
        return False, (f'failures={failures!r}, want the unregistered publish '
                       "point with markers=['write-status']")
    return True, 'ok'


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
    ('linux-visible-census-trusted', SOURCE,
     "    if sys.platform != 'darwin':\n",
     "    if False:  # mutant: trust the platform's visible process set\n"),
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
    ('fixture-summary-relaxed', CHECK,
     "    if (malformed or not executed\n"
     "            or any(row.get('passed') is False for row in executed)):\n"
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
    ('calibrate-param-augassign-opaque', CONTROLS_SOURCE,
     "def run_all(output, source=None):",
     "def publish(report, payload):\n"
     "    report |= payload\n"
     "    return report\n"
     "def run_all(output, source=None):"),
]

JUDGES = {
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
    'calibrate-param-augassign-opaque': (
        'aggregation-audit', calibrate_param_augassign_opaque, False, ()),
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
}
if set(JUDGES) != {row[0] for row in MUTATIONS}:
    raise SystemExit('JUDGES must name exactly the MUTATIONS rows')


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
KILL_GRACE_SECONDS = 10  # negative_process.KILL_TIMEOUT
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
    """
    raise SystemExit(128 + signum)


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
    ok, detail = judge(rows)
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
    signal.signal(signal.SIGTERM, terminate_cleanly)
    if hasattr(signal, 'SIGHUP'):
        signal.signal(signal.SIGHUP, terminate_cleanly)
    known = {row[0] for row in MUTATIONS}
    if set(args.only) - known or len(args.only) != len(set(args.only)):
        common = committed_status_contract(ROOT)
        return common.aggregate_status('BLOCKED')[1]
    selected = ([row for row in MUTATIONS if row[0] in args.only]
                if args.only else list(MUTATIONS))
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
            expected_status = 'PASS' if sys.platform == 'darwin' else 'PASS_WITH_SKIPS'
            expected_code = 0 if expected_status == 'PASS' else 1
            restored_ok = False
            if restored_summary is not None:
                rows = restored_summary.get('results') or []
                wiring = [row for row in rows if row.get('mode') == 'wiring']
                if expected_status == 'PASS':
                    rows_ok = all(row.get('passed') is True for row in rows)
                else:
                    rows_ok = (all(row.get('passed') in (True, None) for row in rows)
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
            # child (non-Darwin platform skips) can never become PASS/0.
            top_status, exit_code = common.aggregate_status(
                *(MUTATION_STATUS.get(entry['verdict'], 'FAIL') for entry in summary),
                restored_summary.get('status') if restored_ok else 'FAIL')
            report = {'status': top_status, 'sourceSha256': digests[SOURCE],
                      'sourceSha256All': {source.relative_to(ROOT).as_posix(): digests[source]
                                          for source in sources},
                      'baseline': args.baseline,
                      'selected': [row[0] for row in selected],
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

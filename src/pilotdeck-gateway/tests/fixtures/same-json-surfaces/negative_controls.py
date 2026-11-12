#!/usr/bin/env python3
"""#2039 same_json surface mutations; memory rehearsal or guarded post-commit run.

Each case reverts one of the six comparisons this batch moved to the strict
``same_json`` walk back to Python's bare ``==``/``!=`` (the exact regression
shape: ``True == 1`` passing a drifted JSON boolean as identical).  The bound
tests are the #2039 pins in ``scripts/gateway-parity/test_safety.py``
(``SameJsonSurfaces``); only that class's own single-line ``#2039`` assertion
counts as a kill.

--memory patches the loader in-process (no source bytes are written, the
shared tree lock is still held for the whole drill); the default mode writes
mutated bytes under ``mutating_sources`` and therefore requires the candidate
sources to equal HEAD -- run it from a committed tree, not this worktree.

Selection is the sibling shape ``NEW_OUTPUT [NAME ...]`` (#2425): names are
positional, so the refusal matrix in ``test_negative_optimize_guards.py``
can drive this entry like every migrated one.
"""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import re
import signal
import time
import unittest
from unittest.mock import patch

root = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (GIT_TIMEOUT, KILL_TIMEOUT, SourceBaselineError,
                              defer_signal_exit, mutating_sources, run_owned,
                              tree_lock, write_source_atomic)
from negative_selection import claim_dir, refuse, registered, select, verdict_exit

session = root / 'scripts/gateway-parity/session_domain.py'
native = root / 'scripts/gateway-parity/native_evidence.py'
test_safety = root / 'scripts/gateway-parity/test_safety.py'
# Mutating any scripts/gateway-parity source puts the run under the shared
# parity lease, and negative_process._parity_mutation_lock refuses to load
# that lease from anything but guarded common.py bytes (#166).
common = root / 'scripts/gateway-parity/common.py'

# #2425 (DS concurrency #2): the guarded window holds the shared parity lease
# (/tmp/pdrs-parity.lock) while product bytes are mutated, and common.MAX_AGE
# is the age at which another apparatus may take that lease over -- fe-parity
# is a bare mkdir taker, so it would read mutated bytes.  Without a whole-run
# deadline the worst case is the whole table times 3 bound commands each
# charged ``timeout + twice the kill grace`` -- the 10 cases the r2 review
# counted already summed to exactly MAX_AGE, and the table has since grown to
# 18.  The budget bounds the window below that age instead: it is checked
# before every case and a spent budget marks the rest INVALID (never KILLED,
# never silently skipped), so the run fails closed instead of outliving the
# lease.
LEASE_BUDGET_SECONDS = 1800
RUN_TIMEOUT_SECONDS = 60
# Imported rather than restated: the reserve below is only correct while this
# number equals the upstream kill grace, and a copy drifts silently (mirror of
# session-domain-cleanup's MiniMax boundary r14 P2-2).
KILL_GRACE_SECONDS = KILL_TIMEOUT
# The guarded set whose bytes the with-statement below snapshots, restores and
# re-verifies: one list, so the exit-side reserve cannot drift from the paths
# the window actually covers.
GUARDED_PATHS = (session, native, common, Path(__file__).resolve())
# What the reserve has to absorb besides the per-run worst case, all inside
# the lease (#2552, DS concurrency#1 -- the old flat 20s charged none of
# this): mutating_sources' exit side rewrites every guarded path atomically
# (one fsync each, charged as slack below) and re-verifies it against HEAD
# through one _git call per path, whose real worst case is GIT_TIMEOUT plus
# twice the kill grace (negative_process._collect pays the grace twice:
# communicate, then wait), and then the parity lease is released.  With the
# four guarded paths that is 4 * (120 + 2 * 10) = 560s plus slack, so a flat
# 20 left the invariant able to understate the true tail by ~28x.
FSYNC_AND_LEASE_SLACK_SECONDS = 20
TAIL_OVERHEAD_SECONDS = (len(GUARDED_PATHS) * (GIT_TIMEOUT + 2 * KILL_GRACE_SECONDS)
                         + FSYNC_AND_LEASE_SLACK_SECONDS)


def lease_budget_left(started, now):
    """Seconds the case loop may still spend before the reserved tail.

    One whole case stays reserved: the one about to start runs up to three
    bound commands (positive, mutant, restored), each charged at its real
    worst case ``timeout + 2 * kill grace`` (negative_process._collect pays
    the grace twice: communicate, then wait).
    """
    reserve = 3 * (RUN_TIMEOUT_SECONDS + 2 * KILL_GRACE_SECONDS) + TAIL_OVERHEAD_SECONDS
    return LEASE_BUDGET_SECONDS - (now - started) - reserve

# (name, file, old bytes, new bytes, bound test).  The old bytes are the
# post-#2039 spelling, so an anchor that no longer matches is drift, not a
# pass: preflight refuses to run and nothing is recorded as SURVIVED.
cases = [
    ('surfaces-frames-bare-not-equal', session,
     "    if not native_evidence.same_json(ts['frames'], rust['frames']):",
     "    if ts['frames'] != rust['frames']:",
     'test_boolean_drift_surfaces_on_frames'),
    ('surfaces-durable-bare-not-equal', session,
     "    if not native_evidence.same_json(ts['durableStatus'], rust['durableStatus']):",
     "    if ts['durableStatus'] != rust['durableStatus']:",
     'test_boolean_drift_surfaces_on_durable_status'),
    ('surfaces-applicable-bare-not-equal', session,
     "    if not native_evidence.same_json(expected.get('applicable'), registered.get('applicable')):",
     "    if expected.get('applicable') != registered.get('applicable'):",
     'test_applicable_drift_surfaces_before_the_field_guards'),
    ('surfaces-registry-fields-bare-not-equal', session,
     "            if not native_evidence.same_json(expected.get(field), registered.get(field)):",
     "            if expected.get(field) != registered.get(field):",
     'test_registry_field_drift_surfaces'),
    ('cleanup-replay-bare-equal', native,
     "    checked['cleanup']['allStopped'] = same_json(evidence['cleanup'], replay['cleanup'])",
     "    checked['cleanup']['allStopped'] = evidence['cleanup'] == replay['cleanup']",
     'test_cleanup_replay_drift_is_not_coerced'),
    ('prelaunch-bare-equal', native,
     "    need(same_json({k: v for k, v in contract.items() if k != 'launcherPid'}, prelaunch),",
     "    need({k: v for k, v in contract.items() if k != 'launcherPid'} == prelaunch,",
     'test_prelaunch_drift_is_not_coerced'),
    ('receipt-tracer-exit-bare-equal', native,
     "    need(receipt['status'] == 'WITNESS_CAPTURED' and same_json(receipt['tracerExitCode'], 0),",
     "    need(receipt['status'] == 'WITNESS_CAPTURED' and receipt['tracerExitCode'] == 0,",
     'test_tracer_exit_code_drift_is_not_coerced'),
    ('ws-profile-opcode-rsv-bare-equal', native,
     "    need(same_json(wire.get('opcode'), 1) and wire.get('fin') is True and same_json(wire.get('rsv'), 0),",
     "    need(wire.get('opcode') == 1 and wire.get('fin') is True and wire.get('rsv') == 0,",
     'test_ws_profile_opcode_drift_is_not_coerced'),
    ('session-profile-connections-bare-equal', native,
     "    need(same_json(contract.get('connections'), 1), 'session profile connections')",
     "    need(contract.get('connections') == 1, 'session profile connections')",
     'test_session_profile_connections_drift_is_not_coerced'),
    ('memory-rehearsal-without-lock', Path(__file__).resolve(),
     '        if memory:\n            with tree_' + 'lock(root, timeout=900):\n'
     '                started = True\n                for row in selected:\n                    one(row)\n        else:',
     '        if memory:\n            for row in selected:\n                one(row)\n        else:',
     'test_memory_rehearsal_holds_tree_lock'),
    # #2431: the startup-refusal contract (rc 2 + withdrawn claim directory)
    # had no committed regression pin; without these self-mutations a later
    # narrowing of the guard's except clause or an early ``started = True``
    # would not turn anything red.  The anchored bytes are split like the row
    # above so the table literal never contains its own anchor contiguously
    # (otherwise preflight counts 2 and refuses the whole run).
    ('startup-refusal-narrowed-exception', Path(__file__).resolve(),
     '    except Exception as error:\n        if not ' + 'started:',
     '    except (SourceBaselineError, TimeoutError) as error:\n        if not ' + 'started:',
     'test_startup_refusal_is_rc2_memory_branch'),
    ('startup-flag-flipped-before-the-guard', Path(__file__).resolve(),
     '    started = False\n    ' + 'try:',
     '    started = True\n    ' + 'try:',
     'test_startup_refusal_is_rc2_guarded_branch'),
    # #2425: the guarded publish/restore and the terminate handlers are
    # reliability debt without a self-mutation that reverts them -- a later
    # ``path.write_text`` or a dropped ``install_terminate_handlers()`` call
    # would not turn anything red either.  Same split rule as above.
    ('atomic-publish-reverted-to-truncating', Path(__file__).resolve(),
     '                write_source_atomic(root, path, changed.' + 'encode())',
     '                path.write_' + 'text(changed)',
     'test_mutant_bytes_publish_in_one_rename'),
    ('atomic-restore-reverted-to-truncating', Path(__file__).resolve(),
     '                write_source_atomic(root, path, original' + 's[path])',
     '                path.write_' + 'bytes(originals[path])',
     'test_mutant_bytes_publish_in_one_rename'),
    ('terminate-handlers-uninstalled', Path(__file__).resolve(),
     '            install_terminate_' + 'handlers()\n',
     '',
     'test_guarded_run_installs_terminate_handlers'),
    # #2552: the budget machinery the r2/r3 batch added had no committed
    # regression pin -- deleting the spent-budget branch or the rc2 wrapper
    # below left SameJsonSurfaces green, and gutting the terminate handler's
    # body turned nothing red while a comment claimed it was pinned.  Same
    # split rule as the rows above so the table never contains its own
    # anchor contiguously.
    ('budget-guard-exception-narrowed', Path(__file__).resolve(),
     '    except Exception as er' + "ror:\n        refuse('lease budget guard cannot read",
     '    except (SourceBaselineError, TimeoutError) as er' + "ror:\n        refuse('lease budget guard cannot read",
     'test_budget_guard_refuses_when_common_is_unreadable'),
    ('budget-check-removed', Path(__file__).resolve(),
     '                    if lease_budget_' + 'left(clock, time.monotonic()) <= 0:',
     '                    if False:',
     'test_lease_budget_marks_spent_cases_invalid'),
    ('terminate-body-gutted', Path(__file__).resolve(),
     '    raise SystemExit(128 + ' + 'signum)',
     '    pass',
     'test_terminate_cleanly_raises_the_signal_exit_code'),
]

# Static roster (#1666): the denominator of this run is a literal in the
# source, never len(cases) -- a dropped, renamed, duplicated or reordered row
# is a startup refusal (rc 2), not a smaller green run.
EXPECTED_NAMES = ('surfaces-frames-bare-not-equal',
                  'surfaces-durable-bare-not-equal',
                  'surfaces-applicable-bare-not-equal',
                  'surfaces-registry-fields-bare-not-equal',
                  'cleanup-replay-bare-equal',
                  'prelaunch-bare-equal',
                  'receipt-tracer-exit-bare-equal',
                  'ws-profile-opcode-rsv-bare-equal',
                  'session-profile-connections-bare-equal',
                  'memory-rehearsal-without-lock',
                  'startup-refusal-narrowed-exception',
                  'startup-flag-flipped-before-the-guard',
                  'atomic-publish-reverted-to-truncating',
                  'atomic-restore-reverted-to-truncating',
                  'terminate-handlers-uninstalled',
                  'budget-guard-exception-narrowed',
                  'budget-check-removed',
                  'terminate-body-gutted')

messages = {
    'surfaces-frames-bare-not-equal': '#2039 boolean drift must surface on frames',
    'surfaces-durable-bare-not-equal': '#2039 boolean drift must surface on durableStatus',
    'surfaces-applicable-bare-not-equal': '#2039 registry.applicable boolean drift must surface',
    'surfaces-registry-fields-bare-not-equal': '#2039 registry.events boolean drift must surface',
    'cleanup-replay-bare-equal': '#2039 cleanup allStopped=1 must not coerce to True',
    'prelaunch-bare-equal': '#2039 boolean drift in the prelaunch mirror is rejected',
    'receipt-tracer-exit-bare-equal': '#2039 tracer exit code boolean drift is rejected',
    'ws-profile-opcode-rsv-bare-equal': '#2039 ws profile opcode boolean drift is rejected',
    'session-profile-connections-bare-equal': '#2039 session profile connections boolean drift is rejected',
    'memory-rehearsal-without-lock': '#2039 same-json memory rehearsal holds the source tree lock',
    'startup-refusal-narrowed-exception': '#2431 the memory branch refuses a startup failure with rc 2',
    'startup-flag-flipped-before-the-guard': '#2431 the guarded branch refuses a startup failure with rc 2',
    'atomic-publish-reverted-to-truncating': '#2425 mutant bytes are published in one atomic rename',
    'atomic-restore-reverted-to-truncating': '#2425 mutant bytes are published in one atomic rename',
    'terminate-handlers-uninstalled': '#2425 the guarded run installs terminate handlers before mutating',
    'budget-guard-exception-narrowed': '#2552 the budget guard refuses when common.py is unreadable',
    'budget-check-removed': '#2552 a spent lease budget marks the remaining cases INVALID',
    'terminate-body-gutted': '#2552 terminate_cleanly exits 128+signum outside the restore window',
}


def bound(test):
    # The pins live in the apparatus test entry's SameJsonSurfaces class;
    # resolve the full name before any run so an unknown test is an error,
    # never a silent pass.
    source = test_safety.read_text()
    if '    def ' + test + '(' not in source:
        raise ValueError('unknown bound test ' + test)
    return test_safety, 'SameJsonSurfaces.' + test


def preflight():
    errors = []
    # The exit-side reserve and the restore window cover exactly GUARDED_PATHS
    # (#2552 r1 DS simplify#1): a case row pointing outside that set would
    # KeyError inside the finally restore and leave mutant bytes behind, so an
    # unguarded target is a preflight refusal, never a run.
    unguarded = sorted(str(row[1]) for row in cases if row[1] not in GUARDED_PATHS)
    if unguarded:
        errors.append('case target outside GUARDED_PATHS: ' + ', '.join(unguarded))
    for name, path, old, new, test in cases:
        source = path.read_text()
        try:
            if source.count(old) != 1:
                raise ValueError('anchor count ' + str(source.count(old)))
            compile(source.replace(old, new, 1), str(path), 'exec')
            bound(test)
        except Exception as error:
            errors.append(name + ': ' + str(error))
    print(json.dumps({'compiled': len(cases) - len(errors), 'errors': errors}))
    return not errors


def memory_run(name):
    row = next(row for row in cases if row[0] == name)
    _, path, old, new, test = row
    changed = path.read_bytes().replace(old.encode(), new.encode(), 1)
    compile(changed, str(path), 'exec')
    original_read = Path.read_bytes
    original_code = importlib.machinery.SourceFileLoader.get_code
    def read(p):
        return changed if p.resolve() == path else original_read(p)
    def code(loader, fullname):
        return compile(changed, str(path), 'exec') if Path(loader.path).resolve() == path else original_code(loader, fullname)
    fixture, full_name = bound(test)
    with patch.object(Path, 'read_bytes', read), patch.object(importlib.machinery.SourceFileLoader, 'get_code', code):
        spec = importlib.util.spec_from_file_location('bound_tests', fixture)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        suite = unittest.defaultTestLoader.loadTestsFromName(full_name, module)
        result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


def classify(result, test, marker):
    log = result.stdout + result.stderr
    if result.returncode == 0 and re.search(r'^OK$', log, re.M):
        return 'SURVIVED'
    # Only the named test's own assertion is a kill; unrelated errors/timeouts
    # and setup failures stay INVALID.
    if (result.returncode == 1 and re.search(r'^FAIL: ' + re.escape(test) + r' \(', log, re.M)
            and re.search(r'^AssertionError:[^\n]*' + re.escape(marker), log, re.M)
            and 'ERROR:' not in log and 'Ran 1 test in ' in log):
        return 'KILLED'
    return 'INVALID'


def terminate_cleanly(signum, _frame):
    """Unwind on SIGTERM/SIGHUP/SIGQUIT so the finally blocks restore the
    sources; the default disposition would leave a mutant in the tree and the
    parity lease held (mirror of session-domain-cleanup's #892 r23 form)."""
    if defer_signal_exit(signum):
        return
    raise SystemExit(128 + signum)


def install_terminate_handlers():
    signal.signal(signal.SIGTERM, terminate_cleanly)
    if hasattr(signal, 'SIGHUP'):
        signal.signal(signal.SIGHUP, terminate_cleanly)
    if hasattr(signal, 'SIGQUIT'):
        signal.signal(signal.SIGQUIT, terminate_cleanly)


def load_pristine_common():
    """Import common.py by explicit path for the constants it publishes.

    ``MAX_AGE`` must come from the bytes the guard itself verifies, not a
    restated copy that drifts silently (mirror of session-domain-cleanup's
    ``load_pristine_common``); the guarded run re-checks these bytes against
    HEAD before the first mutation, and this loader runs before any write.
    """
    spec = importlib.util.spec_from_file_location('same_json_pristine_common', common)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def require_lease_budget_below_max_age():
    """Refuse (rc 2, no evidence directory) when the budget can outlive MAX_AGE.

    An explicit refusal, not an ``assert``: ``python3 -O`` would strip the
    guard and leave only the comment (session-domain-cleanup, GLM
    concurrency r11 P3-4 / MiniMax boundary r11 P3-5).

    #2552 (DS concurrency#2 + boundary#3): a common.py this loader cannot read
    (missing file, syntax error, a renamed MAX_AGE) is itself a startup
    refusal -- this call sits before the with-statement's ``started`` zone,
    so letting it bubble out of main() as a traceback would exit 1 and read
    as a red run, violating the rc2 contract.
    """
    try:
        max_age = load_pristine_common().MAX_AGE
    except Exception as error:
        refuse('lease budget guard cannot read the pristine common constants: '
               + repr(error))
    else:
        if not (LEASE_BUDGET_SECONDS + TAIL_OVERHEAD_SECONDS < max_age):
            refuse('lease budget ' + str(LEASE_BUDGET_SECONDS) + 's + tail overhead '
                   + str(TAIL_OVERHEAD_SECONDS) + 's must stay below common.MAX_AGE '
                   + str(max_age) + 's')


def main():
    if sys.argv[1:2] == ['--memory-run']:
        return memory_run(sys.argv[2])
    try:
        registered(cases, EXPECTED_NAMES)
    except Exception as error:
        refuse('case roster drifted from the static inventory: ' + repr(error))
    arguments = sys.argv[1:]
    memory = '--memory' in arguments
    if not memory:
        # #2795 fix-r4（评审 sibling P1 同族跟改，半修补齐）：preflight() 的锚点读
        # （含 bound() 的 test_safety 读）也要持树锁读——兄弟变异运行器在自己的
        # write 与 restore 之间可能把变异体留在盘上，无锁读会把核对错在它不拥有
        # 的源上；锁忙 / 读失败是拒跑（2）。
        # #2795 fix-r13（评审修 1，DS/MM sibling P1 双谱系同撞）：本 60 s 锁窗只
        # 属于 --check-anchors 与变异臂；--memory 路径不再先取这把锁（先取 60
        # 再取 900 会把 SameJsonSurfaces.test_memory_rehearsal_holds_tree_lock
        # 钉的「tree_lock 恰一次、timeout=900、锁先于 child」打破成两次取锁，
        # 冻结尺子假红），其全表预检折进 rehearsal 自己的 900 s 锁窗（one() 首个
        # case、任何 child 之前一次）。本窗口与下方 mutating_sources 窗口仍是
        # 顺序关系，锁先释放再重取，无同进程嵌套。
        try:
            with tree_lock(root, timeout=60):
                preflight_ok = preflight()
        except (TimeoutError, OSError, UnicodeDecodeError) as error:
            print(f'tree lock or anchor read failed: '
                  f'{type(error).__name__}: {error}', file=sys.stderr)
            return 2
        if not preflight_ok:
            return 2
    # Before the evidence directory is claimed: a drifted budget invariant is
    # "never started", like every other startup refusal (#2425).
    require_lease_budget_below_max_age()
    if sys.argv[1:] == ['--check-anchors']:
        return 0
    arguments = [a for a in arguments if a != '--memory']
    if not arguments:
        refuse('usage: negative_controls.py NEW_OUTPUT [--memory] [NAME ...] | --check-anchors')
    try:
        selected = select(cases, arguments[1:], always=())
    except Exception as error:
        refuse(str(error))
    out = claim_dir(arguments[0])
    results = []
    # #2795 fix-r13（评审修 1）：--memory 的全表锚点预检折进 rehearsal 的
    # 900 s 锁窗——one() 只在锁内的循环里被调，首个 case 的任何 child 之前
    # 先做一次（preflight() 本体不再取锁，读面仍在锁内）。变异臂不进此
    # 分支，预检仍在 main() 的 60 s 锁窗里先于一切 case。
    table_checked = False
    def run(label, command):
        # The same constant the reserve is charged at, so a call-site timeout
        # edit cannot drift past what lease_budget_left reserved (#2425).
        value = run_owned(command, cwd=root, env=None, timeout=RUN_TIMEOUT_SECONDS)
        (out / (label + '.log')).write_text(value.stdout + value.stderr)
        return value
    def one(row, restore=False):
        nonlocal table_checked
        name, path, old, new, test = row
        # #2795 fix-r13（评审修 1）：见 table_checked 的登记注释——--memory 的
        # 全表预检在 rehearsal 锁窗内、首个 child 之前做一次。预检失败或读失败
        # 沿用 #2431 启动拒跑形状（rc 2、撤走空认领目录、不发 summary），不是
        # rc 1 + 一份看起来跑完了的 summary。
        if memory and not table_checked:
            table_checked = True
            try:
                preflight_ok = preflight()
            except (TimeoutError, OSError, UnicodeDecodeError) as error:
                print(f'tree lock or anchor read failed: '
                      f'{type(error).__name__}: {error}', file=sys.stderr)
                startup_refusal(error)
            if not preflight_ok:
                startup_refusal(ValueError('anchor table preflight failed'))
        status, reason = 'INVALID', 'not run'
        fixture, full = bound(test)
        command = [sys.executable, '-I', '-B', str(fixture), full]
        try:
            changed = path.read_text().replace(old, new, 1)
            compile(changed, str(path), 'exec')
            positive = run(name + '-positive', command)
            if positive.returncode != 0 or 'Ran 1 test in ' not in positive.stderr:
                raise ValueError('bound positive failed')
            if memory:
                result = run(name + '-mutant', [sys.executable, '-I', '-B', __file__, '--memory-run', name])
            else:
                # #1903: a truncating in-place write leaves a half-file on
                # SIGKILL; publish in one rename instead.
                write_source_atomic(root, path, changed.encode())
                result = run(name + '-mutant', command)
            status = classify(result, test, messages[name])
            reason = 'compiled; ' + full + '; ' + messages[name]
        except Exception as error:
            reason = repr(error)
        finally:
            if restore:
                write_source_atomic(root, path, originals[path])
        try:
            restored = run(name + '-restored', command)
            if restored.returncode != 0:
                status, reason = 'INVALID', 'restored bound test failed'
        except Exception as error:
            status, reason = 'INVALID', 'restored run failed: ' + repr(error)
        results.append({'name': name, 'status': status, 'reason': reason})
    def startup_refusal(error):
        # A guard that never started (dirty tree, lease or source-lock wait
        # exhausted, git or scanner timeout) is a startup refusal, not a red
        # run: rc 2 and no evidence dir that looks finished
        # (specs/negative-controls/README.md:142/:145/:167).  rmdir only
        # removes the empty dir claim_dir made -- a run that already wrote
        # logs keeps them.
        try:
            out.rmdir()
        except OSError:
            pass
        refuse('guard refused to start: ' + repr(error))

    # Startup is everything the guard does before the first mutation: baseline
    # checks, scanner staging, lease and lock acquisition.  Any exception from
    # that phase is a startup refusal regardless of class -- _git timeouts
    # raise RuntimeError, the workspace scanner raises subprocess.TimeoutExpired
    # / CalledProcessError, tree_lock can raise OSError on top of the
    # TimeoutError already covered -- so the with body's first statement flips
    # ``started`` and the handler below refuses (rc 2) only when the body never
    # began.  Failures after mutations started (body, restore, __exit__)
    # re-raise past the refusal and stay a red run (rc 1), so a restore-phase
    # RuntimeError("restore failed") is never misread as a startup refusal.
    started = False
    try:
        if memory:
            with tree_lock(root, timeout=900):
                started = True
                for row in selected:
                    one(row)
        else:
            # The fixture guards its own bytes too: the memory-rehearsal case
            # mutates this file, so it must be snapshotted for restore.
            # lock_timeout bounds the acquire wait by the same budget the loop
            # below enforces, so the guard itself cannot outlive the lease
            # window it is about to hold (#2425, DS concurrency #2).
            install_terminate_handlers()
            with mutating_sources(root, GUARDED_PATHS,
                                  lock_timeout=LEASE_BUDGET_SECONDS) as originals:
                started = True
                # The clock starts when the lease is held, not when main()
                # started: the budget bounds how long this apparatus can be
                # the one a takeover waits out (common.MAX_AGE).
                clock = time.monotonic()
                for row in selected:
                    if lease_budget_left(clock, time.monotonic()) <= 0:
                        # Never a kill, never silently skipped: the case is
                        # INVALID and the run fails closed (rc 1), with the
                        # sources restored and the lease released inside the
                        # reserved tail.
                        results.append({'name': row[0], 'status': 'INVALID',
                                        'reason': 'batch spent the ' + str(LEASE_BUDGET_SECONDS)
                                                  + 's parity lease budget before this case; re-run '
                                                  + row[0] + ' by name'})
                        continue
                    one(row, True)
    except Exception as error:
        if not started:
            startup_refusal(error)
        results = [{'name': row[0], 'status': 'INVALID', 'reason': 'guard: ' + repr(error)} for row in selected]
    summary = {'mode': 'memory-only' if memory else 'guarded-source', 'results': results,
               'counts': {key: sum(row['status'] == key for row in results) for key in ('KILLED', 'SURVIVED', 'INVALID')}}
    temporary = out / 'summary.json.tmp'
    temporary.write_text(json.dumps(summary, indent=2) + '\n')
    temporary.replace(out / 'summary.json')
    print(json.dumps(summary, indent=2))
    return verdict_exit(results, key='status')


if __name__ == '__main__':
    raise SystemExit(main())

#!/usr/bin/env python3
"""#209 A1/B2 negative controls: release must be gated by proof of absence.

Mutates the frozen parity apparatus itself (Python and shell) and requires the
bound offline test to fail at its own #209 assertion, then restores the bytes.
``mutating_sources`` refuses files that differ from HEAD, so this runner is
executed after the coordinator commits the patch; a local uncommitted tree is
reported as INVALID, never SURVIVED. No cargo or full parity round is launched.

Rows are (name, source, old, new); ``CHECKS`` names the bound unittest and the
exclusive marker its FAIL block must carry. Every Python mutant is compiled and
every shell mutant is ``bash -n`` checked before its bound positive test runs.
All selected rows run to completion; any non-KILLED row is a nonzero exit. A
stale anchor or a failed compile is INVALID and keeps going.

Per-row results live in memory only (#1059). ``summary.json`` (a bare list of
rows, unchanged for its readers) is published exactly once, after
``mutating_sources`` restored every guarded file and verified it byte-for-byte
against the baseline. A failure or cancellation therefore leaves either no
``summary.json`` or an INVALID one; a reader that treats a missing summary as
failure can never observe an all-KILLED file from a cancelled run. No file in
the target directory is deleted or renamed before admission:
``new_output_directory`` refuses a non-empty directory, so a directory holding
an old ``summary.json`` is rejected as-is and a successfully admitted directory
is always empty, which is why a previous success cannot be inherited either
(#1059 fix-r1: a mistyped ``--output`` must keep the previous run's bytes).
The guard exit catches ``BaseException``: a ``KeyboardInterrupt`` or
``SystemExit`` during the restore/re-verify window voids the recorded rows and
publishes INVALID before it is re-raised — every ``SystemExit`` is re-raised
as ``SystemExit(2)``, never by its raw code, so the published INVALID summary
cannot pair with the raw code's process status (256/-256 truncate to a zero
status on POSIX; the #979/#1037 shared semantics, widened in #1059 fix-r2). If
that void publish is itself interrupted (a second SIGINT), no summary is
published at all — missing means failure (fail-closed), not the previous
all-KILLED row set (#1059, the #209 E1 sibling fix).
"""
import argparse
from contextlib import contextmanager, nullcontext
import fcntl
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, new_output_directory, run_owned
from negative_suite import invalidate_completed

ROOT = Path(__file__).resolve().parents[5]
A1_TEST = ROOT / 'scripts/parity/controls/lock_release.py'
ROUND = ROOT / 'scripts/parity/round.py'
B2_TEST = ROOT / 'src/pilotdeck-gateway/tests/fixtures/parity-lock-release/test_lock_release.py'
B2_SHELL = ROOT / 'scripts/ui-server-parity/calibrate.sh'
GATEWAY = ROOT / 'scripts/gateway-parity/common.py'
RUNNER = Path(__file__).resolve()

MUTATIONS = [
    # A1: the shared lease is deleted even when a spawned session group was
    # not proven stopped.
    ('a1-release-ungated', ROUND,
     '                release = not LIVE_GROUPS',
     '                release = True'),
    # A1: kill/wait errors are dropped from allStopped, so a failed cleanup
    # still releases the lock.
    ('a1-cleanup-errors-ignored', ROUND,
     "    return {'allStopped': not errors and not remaining, 'remaining': remaining, 'errors': errors}",
     "    return {'allStopped': not remaining, 'remaining': remaining, 'errors': errors}"),
    # A1: the leader's final bounded wait timeout is ignored, so cleanup is
    # called proven even though the leader was never reaped.
    ('a1-wait-timeout-ignored', ROUND,
     "    except subprocess.TimeoutExpired:\n        errors.append('leader did not exit')",
     "    except subprocess.TimeoutExpired:\n        pass"),
    # A1: the final absence probe's OSError is read as "group gone".
    ('a1-probe-error-gone', ROUND,
     "        except OSError as error:\n"
     "            errors.append(str(error))\n"
     "            remaining.append(proc.pid)\n"
     "            break",
     "        except OSError:\n"
     "            break"),
    # A1: a retained cleanupPending lease is taken over as a dead/expired lock.
    ('a1-takeover-ignores-pending', ROUND,
     "                if reason and not damaged and not (previous and previous.get('cleanupPending')):",
     '                if reason and not damaged:'),
    # A1: SIGINT/SIGTERM are not blocked across spawn and cleanup.
    ('a1-signals-not-deferred', ROUND,
     '    old = signal.pthread_sigmask(signal.SIG_BLOCK, DEFERRED)',
     '    old = signal.pthread_sigmask(signal.SIG_UNBLOCK, DEFERRED)'),
    # A1: acquisition does not publish the fail-closed marker before spawn.
    ('a1-acquisition-pending-false', ROUND,
     "             'cleanupPending': True}",
     "             'cleanupPending': False}"),
    # A1: an interrupt delivered at the acquisition boundary leaves the lease.
    ('a1-release-owner-check-skipped', ROUND,
     "            if read_owner(lock) == owner:\n                release = not LIVE_GROUPS",
     "            if False:\n                release = not LIVE_GROUPS"),
    # B2: the shell helper deletes the lease even when the group audit failed.
    ('b2-release-ungated', B2_SHELL,
     '        release = not LIVE_GROUPS',
     '        release = True'),
    ('b2-cleanup-errors-ignored', B2_SHELL,
     "    return {'allStopped': not errors and not remaining, 'remaining': remaining,",
     "    return {'allStopped': not remaining, 'remaining': remaining,"),
    # B2: the leader's final bounded wait timeout is ignored.
    ('b2-wait-timeout-ignored', B2_SHELL,
     "    except subprocess.TimeoutExpired:\n        errors.append('leader did not exit')",
     "    except subprocess.TimeoutExpired:\n        pass"),
    # B2: the final absence probe's OSError is read as "group gone".
    ('b2-probe-error-gone', B2_SHELL,
     "        except OSError as error:\n"
     "            errors.append(str(error))\n"
     "            remaining.append(proc.pid)\n"
     "            break",
     "        except OSError:\n"
     "            break"),
    ('b2-signals-not-deferred', B2_SHELL,
     '    old = signal.pthread_sigmask(signal.SIG_BLOCK, DEFERRED)',
     '    old = signal.pthread_sigmask(signal.SIG_UNBLOCK, DEFERRED)'),
    ('b2-acquisition-pending-false', B2_SHELL,
     "            'cleanupPending': True}",
     "            'cleanupPending': False}"),
    # B2: the outer acquisition deferral is removed, so a queued signal at the
    # acquire boundary escapes before run()'s try/finally.
    ('b2-run-no-acquisition-defer', B2_SHELL,
     '        with deferred_signals():\n            mine = acquire(lock)',
     '        with __import__("contextlib").nullcontext():\n            mine = acquire(lock)'),
    # B2: an existing retained lease is removed and replaced instead of refused.
    ('b2-takeover-allowed', B2_SHELL,
     '        if lock.exists() or lock.is_symlink():\n'
     '            raise FileExistsError(str(lock))',
     '        if lock.exists() or lock.is_symlink():\n'
     '            shutil.rmtree(lock)'),
    # #1194/#1398: the three apparatus guards against a second signal during
    # KeyboardInterrupt unwinding are removed; the bound test requires the
    # in-flight exception to survive the second signal.
    ('a1-second-signal-not-suppressed', ROUND,
     '        if isinstance(sys.exception(), KeyboardInterrupt):',
     '        if False:'),
    ('b2-second-signal-not-suppressed', B2_SHELL,
     '    if isinstance(sys.exception(), KeyboardInterrupt):',
     '    if False:'),
    ('gateway-second-signal-not-suppressed', GATEWAY,
     '        if isinstance(sys.exception(), KeyboardInterrupt):',
     '        if False:'),
    # #1398: SIGINT routed around the guarded handler is the other half of the
    # change; dropping the registration must turn the bound test red.
    ('a1-sigint-registration-removed', ROUND,
     '    signal.signal(signal.SIGINT, interrupted)\n'
     '    signal.signal(signal.SIGTERM, interrupted)',
     '    signal.signal(signal.SIGTERM, interrupted)'),
    ('b2-sigint-registration-removed', B2_SHELL,
     '    signal.signal(signal.SIGINT, _interrupted)\n'
     '    signal.signal(signal.SIGTERM, _interrupted)',
     '    signal.signal(signal.SIGTERM, _interrupted)'),
    ('gateway-sigint-registration-removed', GATEWAY,
     '    signal.signal(signal.SIGINT, interrupt)\n'
     '    signal.signal(signal.SIGTERM, interrupt)',
     '    signal.signal(signal.SIGTERM, interrupt)'),
    # #1398 P2a: the guard's diagnostic line must not be able to re-break the
    # guard.  Square the try/except away and a closed stderr replaces the
    # in-flight cancellation again.
    ('a1-stderr-write-unguarded', ROUND,
     "            try:\n"
     "                print(f'#1194 second signal {signum} ignored during cleanup',\n"
     "                      file=sys.stderr, flush=True)\n"
     "            except Exception:\n"
     "                pass\n",
     "            print(f'#1194 second signal {signum} ignored during cleanup',\n"
     "                  file=sys.stderr, flush=True)\n"),
    ('b2-stderr-write-unguarded', B2_SHELL,
     "        try:\n"
     "            print(f'#1194 second signal {signum} ignored during cleanup',\n"
     "                  file=sys.stderr, flush=True)\n"
     "        except Exception:\n"
     "            pass\n",
     "        print(f'#1194 second signal {signum} ignored during cleanup',\n"
     "              file=sys.stderr, flush=True)\n"),
    ('gateway-stderr-write-unguarded', GATEWAY,
     "            try:\n"
     "                print(f'#1194 second signal {signum} ignored during cleanup',\n"
     "                      file=sys.stderr, flush=True)\n"
     "            except Exception:\n"
     "                pass\n",
     "            print(f'#1194 second signal {signum} ignored during cleanup',\n"
     "                  file=sys.stderr, flush=True)\n"),
    # #1398 r5: the narrowed tuple is the real production-invariant failure —
    # a reentrant write raises RuntimeError, not OSError/ValueError, and that
    # replaces the in-flight cancellation.  Paired with the *_reentrant_stderr
    # positives below.
    ('a1-stderr-reentrant-unguarded', ROUND,
     '            except Exception:',
     '            except (OSError, ValueError):'),
    ('b2-stderr-reentrant-unguarded', B2_SHELL,
     '        except Exception:',
     '        except (OSError, ValueError):'),
    ('gateway-stderr-reentrant-unguarded', GATEWAY,
     '            except Exception:',
     '            except (OSError, ValueError):'),
    # B2: calibrate.sh gets its unconditional EXIT release back.
    ('b2-shell-unconditional-trap', B2_SHELL,
     'exec python3 -I - "$B2_SCRIPT_DIR/calibrate.py" "$@" <<\'PY\'',
     "mkdir -p /tmp/pdrs-parity.lock\n"
     "trap 'rmdir /tmp/pdrs-parity.lock' EXIT\n"
     'exec python3 -I - "$B2_SCRIPT_DIR/calibrate.py" "$@" <<\'PY\''),
    # The runner guard catches only Exception, so a cancellation
    # (KeyboardInterrupt / SystemExit) during restore skips the INVALID
    # publication and leaves no final summary; the bound test requires the
    # void publish (#1059 P0-2).
    ('runner-baseexception-narrowed', RUNNER,
     "    except BaseException as error:\n"
     "        reason = f'source guard: {type(error).__name__}: {error}'",
     "    except Exception as error:\n"
     "        reason = f'source guard: {type(error).__name__}: {error}'"),
    # Put the pre-fix "publish each row, verify later" order back: record()
    # writes summary.json while the guarded restore is still unverified. A
    # SIGINT in the later void publish then leaves that all-KILLED file behind
    # (#1059 P0, paired mutant for the structural publication).
    ('runner-publish-before-verify', RUNNER,
     "def record(out, results, name, status, reason):\n"
     "    \"\"\"Append a row in memory; the disk summary is published once, later.\n"
     "\n"
     "    The outcome of an earlier row must never be readable as the final result\n"
     "    while the guarded restore is still unverified (#1059).\n"
     "    \"\"\"\n"
     "    results.append({'mutation': name, 'status': status, 'reason': reason})\n"
     "    print(f'{name}: {status}: {reason}', flush=True)",
     "def record(out, results, name, status, reason):\n"
     "    results.append({'mutation': name, 'status': status, 'reason': reason})\n"
     "    print(f'{name}: {status}: {reason}', flush=True)\n"
     "    write_summary(out, results)"),
    # The pre-admission retirement is put back, so a mistyped --output pointing
    # at another run's evidence directory loses that run's summary.json before
    # admission refuses the directory (#1059 fix-r1 P0: evidence loss).
    ('runner-retire-before-admission', RUNNER,
     "    out = new_output_directory(args.output)\n",
     "    raw = Path(args.output).resolve()\n"
     "    (raw / 'summary.json').unlink(missing_ok=True)\n"
     "    out = new_output_directory(args.output)\n"),
    # The raw-code judgment is restored, so a truthy SystemExit code escapes
    # the normalization: 256/-256 exit as a zero process status and a string
    # code exits 1, all with the INVALID summary already on disk (#1059
    # fix-r2, the Codex r2 P1; killed only at a real process boundary).
    ('runner-systemexit-raw-code', RUNNER,
     "        if isinstance(error, SystemExit):\n"
     "            raise SystemExit(2) from error\n",
     "        if isinstance(error, SystemExit) and not error.code:\n"
     "            raise SystemExit(2) from error\n"),
]

CHECKS = {
    'a1-release-ungated': ('LockReleaseControls.test_retains_lock_when_group_unproven',
                           '#209 an unproven group must retain the shared lock'),
    'a1-cleanup-errors-ignored': ('LockReleaseControls.test_cleanup_error_is_unproven',
                                  '#209 kill/wait OSError must leave the group unproven'),
    'a1-wait-timeout-ignored': ('LockReleaseControls.test_wait_timeout_blocks_round',
                                '#209 wait timeout must leave the round BLOCKED'),
    'a1-probe-error-gone': ('LockReleaseControls.test_probe_error_blocks_round',
                            '#209 probe OSError must leave the round BLOCKED'),
    'a1-takeover-ignores-pending': ('LockReleaseControls.test_cleanup_pending_cannot_be_stolen',
                                    '#209 cleanupPending lock must be refused'),
    'a1-signals-not-deferred': ('LockReleaseControls.test_signals_deferred_across_spawn',
                                '#209 SIGINT/SIGTERM must be blocked during spawn/cleanup'),
    'a1-acquisition-pending-false': ('LockReleaseControls.test_acquisition_publishes_pending',
                                     '#209 acquisition must publish cleanupPending before spawn'),
    'a1-release-owner-check-skipped': ('LockReleaseControls.test_acquisition_interrupt_releases_empty_lock',
                                      '#209 cancelled acquisition must release an empty lock'),
    'b2-release-ungated': ('LockGateControls.test_retains_lock_when_group_unproven',
                           '#209 an unproven group must retain the lease'),
    'b2-cleanup-errors-ignored': ('LockGateControls.test_cleanup_error_returns_blocked_and_retains_lease',
                                  '#209 cleanup OSError must return BLOCKED'),
    'b2-wait-timeout-ignored': ('LockGateControls.test_wait_timeout_returns_blocked_and_retains_lease',
                                '#209 wait timeout must return BLOCKED'),
    'b2-probe-error-gone': ('LockGateControls.test_probe_error_returns_blocked_and_retains_lease',
                            '#209 probe OSError must return BLOCKED'),
    'b2-signals-not-deferred': ('LockGateControls.test_signals_deferred_across_spawn',
                                '#209 SIGINT/SIGTERM must be blocked during spawn/cleanup'),
    'b2-acquisition-pending-false': ('LockGateControls.test_acquisition_publishes_pending',
                                     '#209 acquisition must publish cleanupPending before spawn'),
    'b2-run-no-acquisition-defer': ('LockGateControls.test_acquisition_interrupt_releases_empty_lock',
                                    '#209 cancelled acquisition must release an empty lock'),
    'b2-takeover-allowed': ('LockGateControls.test_existing_cleanup_pending_cannot_be_stolen',
                            '#209 B2 never steals a lease'),
    'b2-shell-unconditional-trap': ('LockGateControls.test_shell_has_no_unconditional_release',
                                    '#209 calibrate.sh must not release on every EXIT'),
    'a1-second-signal-not-suppressed': (
        'SignalUnwindControls.test_round_second_signal_ignored_during_cleanup',
        '#1194 round.py must ignore a second signal during cleanup unwinding'),
    'b2-second-signal-not-suppressed': (
        'SignalUnwindControls.test_calibrate_second_signal_ignored_during_cleanup',
        '#1194 calibrate.sh must ignore a second signal during cleanup unwinding'),
    'gateway-second-signal-not-suppressed': (
        'SignalUnwindControls.test_gateway_second_signal_ignored_during_cleanup',
        '#1194 common.py must ignore a second signal during cleanup unwinding'),
    'a1-sigint-registration-removed': (
        'SignalUnwindControls.test_round_second_signal_ignored_during_cleanup',
        '#1194 round.py must route SIGINT and SIGTERM to one handler'),
    'b2-sigint-registration-removed': (
        'SignalUnwindControls.test_calibrate_second_signal_ignored_during_cleanup',
        '#1194 calibrate.sh must route SIGINT and SIGTERM to one handler'),
    'gateway-sigint-registration-removed': (
        'SignalUnwindControls.test_gateway_second_signal_ignored_during_cleanup',
        '#1194 common.py must route SIGINT and SIGTERM to one handler'),
    'a1-stderr-write-unguarded': (
        'SignalUnwindControls.test_round_guard_survives_broken_stderr',
        '#1194 a failing stderr must not re-break the guard'),
    'b2-stderr-write-unguarded': (
        'SignalUnwindControls.test_calibrate_guard_survives_broken_stderr',
        '#1194 calibrate.sh must survive a failing stderr'),
    'gateway-stderr-write-unguarded': (
        'SignalUnwindControls.test_gateway_guard_survives_broken_stderr',
        '#1194 common.py must survive a failing stderr'),
    'a1-stderr-reentrant-unguarded': (
        'SignalUnwindControls.test_round_guard_survives_reentrant_stderr',
        '#1194 a reentrant stderr must not re-break the guard'),
    'b2-stderr-reentrant-unguarded': (
        'SignalUnwindControls.test_calibrate_guard_survives_reentrant_stderr',
        '#1194 calibrate.sh must survive a reentrant stderr'),
    'gateway-stderr-reentrant-unguarded': (
        'SignalUnwindControls.test_gateway_guard_survives_reentrant_stderr',
        '#1194 common.py must survive a reentrant stderr'),
    'runner-baseexception-narrowed': (
        'RunnerInvalidationControls.test_final_guard_interrupt_rewrites_summary',
        '#1059 P0-2 a cancellation after the last row must not keep KILLED'),
    'runner-publish-before-verify': (
        'RunnerInvalidationControls.test_interrupt_during_void_publish_leaves_no_killed_summary',
        '#1059 P0-3 an interrupt during the void publish must not leave a readable KILLED summary'),
    'runner-retire-before-admission': (
        'RunnerInvalidationControls.test_existing_evidence_is_refused_byte_identical',
        '#1059 P0-4 a pre-existing evidence directory must be refused with every byte preserved'),
    'runner-systemexit-raw-code': (
        'RunnerInvalidationControls.test_invalid_exit_normalizes_process_status',
        '#1059 fix-r2 an INVALID summary must not pair with the raw SystemExit process status'),
}


LOCK = Path('/tmp/pdrs-parity.lock')


def _owner(lock):
    try:
        return json.loads((lock / 'owner.json').read_text())
    except (OSError, ValueError, TypeError, KeyError):
        return None


def _publish(path, value):
    temporary = path.with_name(path.name + f'.tmp.{os.getpid()}')
    temporary.write_text(json.dumps(value) + '\n')
    temporary.replace(path)


# #2929 fix-r2: the parity-lease acquire budget for this runner: the shared
# branch's mutating_sources window and the non-shared branch's own
# parity_lease() both acquire the same host-wide /tmp/pdrs-parity.lock.
# FAMILY_MAX -- the family roster's largest whole-round hold bound
# (gateway-batch; the live value is the roster's FAMILY_MAX) -- plus this runner's own restore tail and one
# stage of handoff margin, rounded up to the 900 s family unit.  One
# derivation for the whole family, recomputed live in
# test_lease_acquire_budgets.py; its closed roster keeps a new global-lease
# runner from silently missing a budget.  A live holder always carries
# cleanupPending and no taker takes such an owner over, so the budget
# deliberately exceeds common.MAX_AGE -- MAX_AGE only ages out
# never-published stale locks; a published owner, dead pid included, keeps
# the cleanupPending protection and is never taken over automatically
# (fix-r3: such a residual now fails the acquire fast with a diagnostic
# instead of waiting the budget out) -- and just has to cover the family's
# largest live hold (#1528/#1529).
LEASE_ACQUIRE_BUDGET_SECONDS = 63900


@contextmanager
def parity_lease(lock=LOCK, timeout=LEASE_ACQUIRE_BUDGET_SECONDS):
    """Hold the family directory lease while mutating frozen apparatus.

    The control runner is not product code, so this is a small, fail-closed
    acquisition of the same /tmp/pdrs-parity.lock + .guard protocol. It never
    steals a held lease; a busy lock is INVALID, not a mutation window.
    """
    mine = {'pid': os.getpid(), 'timestamp': time.time(), 'token': uuid.uuid4().hex,
            'cleanupPending': True}
    guard = lock.with_name(lock.name + '.guard')
    deadline = time.monotonic() + timeout
    with guard.open('a') as handle:
        while True:
            try:
                fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() >= deadline:
                    raise TimeoutError(f'#209 parity lock guard busy: {lock}')
                time.sleep(.1)
        try:
            if lock.exists():
                raise TimeoutError(f'#209 parity lock held: {lock}')
            lock.mkdir()
            try:
                _publish(lock / 'owner.json', mine)
            except BaseException:
                shutil.rmtree(lock)
                raise
            yield
        finally:
            if lock.exists() and _owner(lock) == mine:
                shutil.rmtree(lock)
            fcntl.flock(handle, fcntl.LOCK_UN)


def require_single_anchor(original, old, name):
    if original.count(old) != 1:
        raise ValueError(f'#{name} anchor expired: count={original.count(old)}')


def script_for(test):
    return A1_TEST if test.startswith('LockReleaseControls.') else B2_TEST


def run(out, label, test):
    script = script_for(test)
    result = run_owned([sys.executable, '-I', '-B', str(script), test, '-v'],
                       cwd=ROOT, timeout=180)
    (out / (label + '.log')).write_text(result.stdout + result.stderr)
    return result


def compile_mutant(name, path, mutant):
    if path.suffix == '.sh':
        result = subprocess.run(['bash', '-n'], input=mutant, text=True,
                                capture_output=True, timeout=30)
        if result.returncode:
            raise ValueError(f'bash -n failed: {result.stderr.strip()}')
        marker = "<<'PY'\n"
        if marker in mutant:
            start = mutant.index(marker) + len(marker)
            end = mutant.index('\nPY\n', start)
            compile(mutant[start:end] + '\n', str(path) + ':inline', 'exec')
        return 'bash -n: PASS; inline Python compile: PASS\n'
    compile(mutant, str(path), 'exec')
    return 'Python compile: PASS\n'


def _counts(results):
    return {state: sum(row['status'] == state for row in results)
            for state in ('KILLED', 'SURVIVED', 'INVALID')}


def write_summary(out, results):
    """Publish the one final summary atomically.

    This is called exactly once per normal run, after every guarded file was
    restored and byte-verified, and once from the failure handler (INVALID).
    The payload stays the bare list of rows this suite's readers expect; a
    reader that checks "the file exists" must never observe a half-written
    JSON document, so the payload goes to a sibling temp file and is renamed
    over ``summary.json`` (#166 r8).
    """
    payload = json.dumps(results, indent=2) + '\n'
    target = out / 'summary.json'
    tmp = out / 'summary.json.tmp'
    tmp.write_text(payload)
    tmp.replace(target)


def record(out, results, name, status, reason):
    """Append a row in memory; the disk summary is published once, later.

    The outcome of an earlier row must never be readable as the final result
    while the guarded restore is still unverified (#1059).
    """
    results.append({'mutation': name, 'status': status, 'reason': reason})
    print(f'{name}: {status}: {reason}', flush=True)


def check_anchors(selected):
    for name, source, old, _new in selected:
        text = source.read_text()
        count = text.count(old)
        if count != 1:
            print(f'STALE ANCHOR {name}: {count} matches in {source.relative_to(ROOT)}')
            return 2
    print(f'{len(selected)} anchors: ok')
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--only', action='append')
    parser.add_argument('--check-anchors', action='store_true')
    parser.add_argument('--baseline', default='HEAD',
                        help='tree-ish the guarded sources must equal; HEAD for '
                             'the delivered post-commit run (a dirty rehearsal may '
                             'pass git stash create)')
    args = parser.parse_args()
    known = {row[0] for row in MUTATIONS}
    if args.only and (set(args.only) - known or len(args.only) != len(set(args.only))):
        print('unknown or duplicate --only name', file=sys.stderr)
        return 2
    selected = [row for row in MUTATIONS if not args.only or row[0] in args.only]
    if args.check_anchors:
        return check_anchors(selected)
    if args.output is None:
        print('--output is required unless --check-anchors is used', file=sys.stderr)
        return 2
    # #1059 fix-r1 P0: nothing in the target directory is touched before
    # admission.  new_output_directory refuses any non-empty directory, so a
    # directory that still holds an old summary.json (or any other evidence)
    # exits with the #953 refusal and keeps every byte; a mistyped --output
    # therefore cannot delete a previous run's result.  Since admission only
    # succeeds on an empty directory, no stale summary can be inherited either.
    out = new_output_directory(args.output)
    results = []
    try:
        paths = list(dict.fromkeys([RUNNER, *(row[1] for row in selected)]))
        # #209 negative controls mutate frozen apparatus: hold the same
        # /tmp/pdrs-parity.lock the real runs use, loaded from baseline bytes.
        # RUNNER is guarded too because the runner rows mutate this delivered
        # runner to prove the cancellation regression bites (#1059).
        # A scripts/gateway-parity source makes mutating_sources take that same
        # lease itself, from verified bytes. The lease is not reentrant: holding
        # it here as well blocks on /tmp/pdrs-parity.lock.guard against our own
        # process until the acquire budget's bound, voiding every row (#1398).
        # The non-shared branch's own parity_lease() does NOT wait the host-wide
        # lease out: it polls only the .guard flock (a lease-holding peer
        # leaves that guard free), and the first lease-directory check after
        # it fails immediately (#2929 fix-r3: fail-fast -- a queued --only
        # run INVALIDs in seconds instead of waiting; the family budget
        # bounds only the millisecond-scale guard contention).
        shared = any((ROOT / path).resolve().is_relative_to(GATEWAY.parent)
                     for path in paths)
        lease = nullcontext() if shared else parity_lease()
        with lease, mutating_sources(ROOT, paths, baseline=args.baseline,
                                    lock_timeout=LEASE_ACQUIRE_BUDGET_SECONDS) as originals:
            for name, source, old, new in selected:
                status, reason = 'INVALID', 'not executed'
                test, message = CHECKS[name]
                try:
                    original = originals[source].decode()
                    require_single_anchor(original, old, name)
                    mutant = original.replace(old, new, 1)
                    (out / (name + '-compile.log')).write_text(
                        compile_mutant(name, source, mutant))
                    positive = run(out, name + '-positive', test)
                    if positive.returncode != 0 or 'Ran 1 test' not in positive.stderr:
                        raise RuntimeError('bound positive test did not pass')
                    source.write_text(mutant)
                    negative = run(out, name + '-negative', test)
                    text = negative.stdout + negative.stderr
                    if negative.returncode == 0:
                        status, reason = 'SURVIVED', 'bound test passed the mutant'
                    elif (('FAIL: ' + test.split('.')[-1]) in text and message in text
                          and 'ERROR:' not in text):
                        status, reason = 'KILLED', message
                    else:
                        status, reason = 'INVALID', 'failed outside bound assertion: ' + message
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    source.write_bytes(originals[source])
                try:
                    restored = run(out, name + '-restored', test)
                    if restored.returncode != 0:
                        status, reason = 'INVALID', reason + '; restored positive failed'
                except Exception as error:
                    status, reason = 'INVALID', reason + f'; restoration test: {error}'
                record(out, results, name, status, reason)
    except BaseException as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        # #979/#1037 + #1059: a guard/restoration failure voids every
        # already-recorded row in memory, then publishes INVALID. The disk
        # never held a final summary before this point (record() is memory
        # only), so an interrupt inside this publish — even a second SIGINT —
        # leaves no summary at all instead of the old all-KILLED file. Missing
        # summary is the fail-closed signal.
        invalidate_completed(results, reason)
        for name, *_ in selected[len(results):]:
            record(out, results, name, 'INVALID', reason)
        write_summary(out, results)
        # The INVALID summary is on disk, so the exit is normalized instead of
        # judged by the raw code: 256/-256 truncate to a zero process status on
        # POSIX and a string code exits 1, none of which may decide how a
        # coordinator reads this run (#1059 fix-r2, the Codex r2 P1).
        if isinstance(error, SystemExit):
            raise SystemExit(2) from error
        if not isinstance(error, Exception):
            raise
        return 2
    # Reaching here means mutating_sources restored every guarded file and
    # verified it against the baseline. Only now may a final summary exist.
    write_summary(out, results)
    counts = _counts(results)
    print(json.dumps(counts), flush=True)
    return 0 if counts['KILLED'] == len(results) else 1


if __name__ == '__main__':
    raise SystemExit(main())

#!/usr/bin/env python3
"""#209 E1 negative controls: release must be gated by proof of absence.

Mutates the frozen gateway apparatus in place (``session_domain.py`` and the
shared ``common.py`` lease protocol) and requires the bound offline test to
fail at its own #209 assertion, then restores the bytes.  ``mutating_sources``
refuses files that differ from HEAD, so this runner is executed only after the
coordinator commits the patch; a local uncommitted tree is INVALID, never
SURVIVED.  No cargo or full calibration is launched.

Rows are ``(name, source, old, new)``; ``CHECKS`` names the bound unittest and
the exclusive marker its FAIL block must carry.  Every mutant is compiled
before its bound positive runs.  All selected rows run to completion; any
non-KILLED row is a nonzero exit.  A stale anchor or a failed compile is
INVALID and keeps going.  If the outer source guard or restoration fails, every
already-recorded row is invalidated and an INVALID summary is published (the
#979/#1037 shared semantics) instead of keeping a false green summary.

Per-row results live in memory only.  ``summary.json`` is published exactly
once, after ``mutating_sources`` restored every guarded file and verified it
byte-for-byte against the baseline.  A failure or cancellation therefore leaves
either no ``summary.json`` or an INVALID one; a reader that treats a missing
summary as failure can never observe an all-KILLED file from a cancelled run.
No file in the target directory is deleted or renamed before admission:
``new_output_directory`` refuses a non-empty directory, so a directory holding
an old ``summary.json`` is rejected as-is and a successfully admitted directory
is always empty, which is why a previous success cannot be inherited (#209 E1
r4 P0: a mistyped ``--output`` must keep the previous run's bytes).  The guard
exit catches ``BaseException``: a ``KeyboardInterrupt`` or ``SystemExit`` during
the restore/re-verify window voids the recorded rows and publishes INVALID
before it is re-raised.  If that void publish is itself interrupted (a second
SIGINT), no summary is published at all — missing means failure (fail-closed),
not the previous all-KILLED row set (#209 E1 P0).
"""
import argparse
import json
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, new_output_directory, run_owned
from negative_suite import invalidate_completed

ROOT = Path(__file__).resolve().parents[5]
SESSION = ROOT / 'scripts/gateway-parity/session_domain.py'
COMMON = ROOT / 'scripts/gateway-parity/common.py'
RUNNER = Path(__file__).resolve()
TEST = Path(__file__).resolve().with_name('test_lock_release.py')

MUTATIONS = [
    # The lease is deleted even when a spawned group was not proven stopped.
    ('e1-release-ungated', SESSION,
     "                    if verified:\n"
     "                        common.LIVE_GROUPS.discard(child.pid)",
     "                    if True:\n"
     "                        common.LIVE_GROUPS.discard(child.pid)"),
    # Spawn and registration no longer share a deferred-signal window.
    ('e1-spawn-not-deferred', SESSION,
     "            with deferred_signals():\n"
     "                child = subprocess.Popen(command, cwd=ROOT, stdout=log,",
     "            with __import__('contextlib').nullcontext():\n"
     "                child = subprocess.Popen(command, cwd=ROOT, stdout=log,"),
    # Cleanup no longer defers SIGINT/SIGTERM.
    ('e1-cleanup-not-deferred', SESSION,
     "        with deferred_signals():\n"
     "            try:\n"
     "                if child is not None:\n"
     "                    _cleanup_fixture(child, audit, leader_state, output)",
     "        with __import__('contextlib').nullcontext():\n"
     "            try:\n"
     "                if child is not None:\n"
     "                    _cleanup_fixture(child, audit, leader_state, output)"),
    # The owned pgid is never registered, so the lock cannot see it.
    ('e1-group-not-registered', SESSION,
     "                common.LIVE_GROUPS.add(child.pid)",
     "                pass  # mutant: the owned group is not registered"),
    # A probe OSError is published as VERIFIED and its error is dropped.
    ('e1-probe-error-as-verified', SESSION,
     "        except (OSError, subprocess.SubprocessError) as error:\n"
     "            audit['cleanupBlocked'] = f'group probe raised: {error}'\n"
     "            audit['processGroupProbe'] = {\n"
     "                'pgid': child.pid, 'status': CLEANUP_NOT_VERIFIED,\n"
     "                'reason': f'group probe raised: {type(error).__name__}: {error}'}",
     "        except (OSError, subprocess.SubprocessError) as error:\n"
     "            audit['processGroupProbe'] = {\n"
     "                'pgid': child.pid, 'status': CLEANUP_VERIFIED,\n"
     "                'reason': f'group probe raised: {type(error).__name__}: {error}'}"),
    # A leader that was not reaped is still reported VERIFIED.
    ('e1-leader-not-reaped-verified', SESSION,
     "    else:\n"
     "        audit['processTreeCleanup'] = CLEANUP_NOT_VERIFIED",
     "    else:\n"
     "        audit['processTreeCleanup'] = CLEANUP_VERIFIED"),
    # A cleanup error (refused leader kill) is ignored by the release gate.
    ('e1-cleanup-blocked-ignored', SESSION,
     "                                and not audit.get('cleanupBlocked')\n"
     "                                and not audit.get('cleanupRaised'))",
     "                                and not audit.get('cleanupRaised'))"),
    # A cleanup helper that raised after the leader settled is ignored by the
    # release gate (the P0-1 regression: an aborted cleanup must retain).
    ('e1-cleanup-raised-ignored', SESSION,
     "                                and not audit.get('cleanupBlocked')\n"
     "                                and not audit.get('cleanupRaised'))",
     "                                and not audit.get('cleanupBlocked'))"),
    # The EXITED-zombie exemption is put back, so a refused kill of an unreaped
    # leader releases the lease even though the helper did not prove absence.
    ('e1-exited-refused-exempted', SESSION,
     "        elif outcome == 'refused':\n"
     "            audit['cleanupBlocked'] = f'leader kill refused: state {detail}'",
     "        elif outcome == 'refused' and detail != 'EXITED':\n"
     "            audit['cleanupBlocked'] = f'leader kill refused: state {detail}'"),
    # A retained cleanupPending lease is taken over as a dead/expired lock.
    ('e1-takeover-ignores-pending', COMMON,
     "                    if reason and not damaged and not (previous and previous.get('cleanupPending')):",
     "                    if reason and not damaged:"),
    # The lock-lifecycle field is published optimistically even though the
    # owned group was not proven stopped.
    ('e1-allstopped-optimistic', SESSION,
     "                    audit['allStopped'] = verified\n"
     "                    if verified:",
     "                    audit['allStopped'] = True\n"
     "                    if verified:"),
    # The P0-1 regression: a verified probe is downgraded to NOT_VERIFIED
    # whenever the leader was not reaped or a cleanup error was recorded.
    ('e1-scoring-downgrade-restored', SESSION,
     "                    audit['allStopped'] = verified\n"
     "                    if verified:",
     "                    if (audit.get('processTreeCleanup') == CLEANUP_VERIFIED\n"
     "                            and not verified):\n"
     "                        audit['processTreeCleanup'] = CLEANUP_NOT_VERIFIED\n"
     "                    audit['allStopped'] = verified\n"
     "                    if verified:"),
    # The runner guard catches only Exception, so a cancellation (KeyboardInterrupt
    # / SystemExit) during restore skips the INVALID publication and leaves no
    # final summary; the bound test requires the void publish (#209 P0-2).
    ('e1-runner-baseexception-narrowed', RUNNER,
     "    except BaseException as error:\n"
     "        reason = f'source guard: {type(error).__name__}: {error}'",
     "    except Exception as error:\n"
     "        reason = f'source guard: {type(error).__name__}: {error}'"),
    # Put the pre-fix "publish each row, verify later" order back: record()
    # writes summary.json while the guarded restore is still unverified.  A
    # SIGINT in the later void publish then leaves that all-KILLED file behind
    # (#209 E1 P0, paired mutant for the structural publication).
    ('e1-runner-publish-before-verify', RUNNER,
     "def record(out, results, name, status, reason):\n"
     "    \"\"\"Append a row in memory; the disk summary is published once, later.\n"
     "\n"
     "    The outcome of an earlier row must never be readable as the final result\n"
     "    while the guarded restore is still unverified (#209 E1 P0).\n"
     "    \"\"\"\n"
     "    results.append({'mutation': name, 'status': status, 'reason': reason})\n"
     "    print(f'{name}: {status}: {reason}', flush=True)",
     "def record(out, results, name, status, reason):\n"
     "    results.append({'mutation': name, 'status': status, 'reason': reason})\n"
     "    print(f'{name}: {status}: {reason}', flush=True)\n"
     "    write_summary(out, results)"),
    # The pre-admission retirement is put back, so a mistyped --output pointing
    # at another run's evidence directory loses that run's summary.json before
    # admission refuses the directory (#209 E1 r4 P0: evidence loss).
    ('e1-runner-retire-before-admission', RUNNER,
     "    out = new_output_directory(args.output)\n",
     "    raw = Path(args.output).resolve()\n"
     "    (raw / 'summary.json').unlink(missing_ok=True)\n"
     "    out = new_output_directory(args.output)\n"),
]

CHECKS = {
    'e1-release-ungated': (
        'LockReleaseControls.test_unverified_cleanup_retains_lock',
        '#209 unverified cleanup must keep the owned group registered'),
    'e1-spawn-not-deferred': (
        'LockReleaseControls.test_spawn_defers_signals',
        '#209 spawn and registration must block SIGINT/SIGTERM'),
    'e1-cleanup-not-deferred': (
        'LockReleaseControls.test_cleanup_defers_signals',
        '#209 cleanup must block SIGINT/SIGTERM'),
    'e1-group-not-registered': (
        'LockReleaseControls.test_group_registered_before_cleanup',
        '#209 the owned group must be registered before cleanup'),
    'e1-probe-error-as-verified': (
        'LockReleaseControls.test_probe_oserror_retains_lock',
        '#209 probe OSError must leave cleanup NOT_VERIFIED'),
    'e1-leader-not-reaped-verified': (
        'LockReleaseControls.test_leader_not_reaped_retains_lock',
        '#209 a leader that was not reaped must not be verified'),
    'e1-cleanup-blocked-ignored': (
        'LockReleaseControls.test_leader_kill_refusal_retains_lock',
        '#209 a cleanup error must deny allStopped'),
    'e1-cleanup-raised-ignored': (
        'LockReleaseControls.test_cleanup_raised_retains_lock',
        '#209 a cleanup helper error must deny allStopped'),
    'e1-exited-refused-exempted': (
        'LockReleaseControls.test_exited_refused_kill_retains_lock',
        '#209 a refused EXITED kill must be recorded'),
    'e1-takeover-ignores-pending': (
        'LockReleaseControls.test_takeover_pending_refused',
        '#209 cleanupPending lock must be refused'),
    'e1-allstopped-optimistic': (
        'LockReleaseControls.test_probe_oserror_retains_lock',
        '#209 probe OSError must not report allStopped'),
    'e1-scoring-downgrade-restored': (
        'LockReleaseControls.test_leader_kill_refusal_retains_lock',
        '#209 P0-1 a verified probe must keep main scoring (no downgrade)'),
    'e1-runner-baseexception-narrowed': (
        'RunnerInvalidationControls.test_final_guard_interrupt_rewrites_summary',
        '#209 P0-2 a cancellation after the last row must not keep KILLED'),
    'e1-runner-publish-before-verify': (
        'RunnerInvalidationControls.test_interrupt_during_void_publish_leaves_no_killed_summary',
        '#209 P0-3 an interrupt during the void publish must not leave a readable KILLED summary'),
    'e1-runner-retire-before-admission': (
        'RunnerInvalidationControls.test_existing_evidence_is_refused_byte_identical',
        '#209 P0-4 a pre-existing evidence directory must be refused with every byte preserved'),
}


def require_single_anchor(original, old, name):
    if original.count(old) != 1:
        raise ValueError(f'#{name} anchor expired: count={original.count(old)}')


def run(out, label, test):
    result = run_owned([sys.executable, '-I', '-B', str(TEST), test, '-v'],
                       cwd=ROOT, timeout=180)
    (out / (label + '.log')).write_text(result.stdout + result.stderr)
    return result


def compile_mutant(name, path, mutant):
    try:
        compile(mutant, str(path), 'exec')
    except SyntaxError as error:
        raise ValueError(f'{name}: Python compile failed: {error}') from error
    return 'Python compile: PASS\n'


def _counts(results):
    return {state: sum(row['status'] == state for row in results)
            for state in ('KILLED', 'SURVIVED', 'INVALID')}


def write_summary(out, results):
    """Publish the one final summary atomically.

    This is called exactly once per normal run, after every guarded file was
    restored and byte-verified, and once from the failure handler (INVALID).
    A reader that checks "the file exists" must never observe a half-written
    JSON document, so the payload goes to a sibling temp file and is renamed
    over ``summary.json`` (#166 r8).
    """
    payload = json.dumps({'controls': results, 'counts': _counts(results)},
                         indent=2) + '\n'
    target = out / 'summary.json'
    tmp = out / 'summary.json.tmp'
    tmp.write_text(payload)
    tmp.replace(target)


def record(out, results, name, status, reason):
    """Append a row in memory; the disk summary is published once, later.

    The outcome of an earlier row must never be readable as the final result
    while the guarded restore is still unverified (#209 E1 P0).
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
    # #209 E1 r4 P0: nothing in the target directory is touched before
    # admission.  new_output_directory refuses any non-empty directory, so a
    # directory that still holds an old summary.json (or any other evidence)
    # exits with the #953 refusal and keeps every byte; a mistyped --output
    # therefore cannot delete a previous run's result.  Since admission only
    # succeeds on an empty directory, no stale summary can be inherited either.
    out = new_output_directory(args.output)
    results = []
    try:
        # One lease only: mutating_sources acquires the real
        # /tmp/pdrs-parity.lock through common.parity_lock.  A second outer
        # lease would make this non-reentrant guard wait on itself (#209 P1).
        # The protected set always carries common.py: _parity_mutation_lock
        # refuses a gateway-parity path without it, including an SESSION-only
        # --only selection.  RUNNER is guarded too because one row mutates this
        # delivered runner to prove the cancellation regression bites.
        paths = [SESSION, COMMON, RUNNER]
        with mutating_sources(ROOT, paths, baseline=args.baseline) as originals:
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
        # #979/#1037 + #209 E1 P0: a guard/restoration failure voids every
        # already-recorded row in memory, then publishes INVALID.  The disk
        # never held a final summary before this point (record() is memory
        # only), so an interrupt inside this publish — even a second SIGINT —
        # leaves no summary at all instead of the old all-KILLED file.  Missing
        # summary is the fail-closed signal.
        invalidate_completed(results, reason)
        for name, *_ in selected[len(results):]:
            record(out, results, name, 'INVALID', reason)
        write_summary(out, results)
        if not isinstance(error, Exception):
            raise
        return 2
    # Reaching here means mutating_sources restored every guarded file and
    # verified it against the baseline.  Only now may a final summary exist.
    write_summary(out, results)
    counts = _counts(results)
    print(json.dumps(counts), flush=True)
    return 0 if counts['KILLED'] == len(results) else 1


if __name__ == '__main__':
    raise SystemExit(main())

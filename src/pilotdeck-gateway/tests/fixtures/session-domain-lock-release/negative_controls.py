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
already-recorded row is invalidated (the #979 shared semantics) instead of
publishing a false green summary.
"""
import argparse
from contextlib import contextmanager
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

try:  # #979 moves this into the shared suite; use it once it is present.
    from negative_suite import invalidate_completed as _shared_invalidate_completed
except ImportError:
    _shared_invalidate_completed = None

ROOT = Path(__file__).resolve().parents[5]
SESSION = ROOT / 'scripts/gateway-parity/session_domain.py'
COMMON = ROOT / 'scripts/gateway-parity/common.py'
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
     "                                and not audit.get('cleanupBlocked'))",
     "                                and True)"),
    # A retained cleanupPending lease is taken over as a dead/expired lock.
    ('e1-takeover-ignores-pending', COMMON,
     "                    if reason and not damaged and not (previous and previous.get('cleanupPending')):",
     "                    if reason and not damaged:"),
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
        '#209 a cleanup error must deny verification'),
    'e1-takeover-ignores-pending': (
        'LockReleaseControls.test_takeover_pending_refused',
        '#209 cleanupPending lock must be refused'),
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


@contextmanager
def parity_lease(lock=LOCK, timeout=60):
    """Hold the family directory lease while mutating frozen apparatus.

    The control runner is not product code, so this is a small, fail-closed
    acquisition of the same /tmp/pdrs-parity.lock + .guard protocol.  It never
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


def record(out, results, name, status, reason):
    results.append({'mutation': name, 'status': status, 'reason': reason})
    print(f'{name}: {status}: {reason}', flush=True)
    (out / 'summary.json').write_text(json.dumps(
        {'controls': results, 'counts': {
            state: sum(row['status'] == state for row in results)
            for state in ('KILLED', 'SURVIVED', 'INVALID')}}, indent=2) + '\n')


def invalidate_completed(results, reason):
    """A failed source guard invalidates every row already recorded.

    Uses the shared #979 helper when this tree has it; otherwise the identical
    local semantics: the guard restores and verifies every mutated file on exit.
    If it raises there, a previously recorded KILLED row is no longer
    trustworthy and must not survive in the summary.
    """
    if _shared_invalidate_completed is not None:
        return _shared_invalidate_completed(results, reason)
    for row in results:
        row.update(status='INVALID', reason=row['reason'] + '; ' + reason)


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
    out = new_output_directory(args.output)
    results = []
    try:
        paths = list(dict.fromkeys([*(row[1] for row in selected)]))
        # #209 negative controls mutate frozen apparatus: hold the same
        # /tmp/pdrs-parity.lock the real runs use, loaded from HEAD bytes.
        with parity_lease(), mutating_sources(ROOT, paths) as originals:
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
    except Exception as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for name, *_ in selected[len(results):]:
            record(out, results, name, 'INVALID', reason)
        return 2
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    print(json.dumps(counts), flush=True)
    return 0 if counts['KILLED'] == len(results) else 1


if __name__ == '__main__':
    raise SystemExit(main())

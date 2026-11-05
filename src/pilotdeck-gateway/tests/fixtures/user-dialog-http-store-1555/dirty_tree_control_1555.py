#!/usr/bin/env python3
# anchor-selfcheck: unsupported run-level negative control: holds the shared
# tree lock while the peak-RSS driver starts (the driver must stay blocked), and
# dirties one tracked source byte inside that same hold (the driver must refuse).
"""#1555 fix-r2 negative control: the peak-RSS driver takes the tree lock.

Two checks, one per half of the fix:

1. `lock-held` — on a clean tree the driver must first finish `status=OK` with
   a real minimum (`positive` below, so a driver that merely refuses everything
   cannot pass this control). The control then takes the same lock and starts
   the driver: it prints `starting`, then `lock-requested` immediately before
   the blocking `flock`, then `lock-acquired` as the first statement after the
   request returns. Seeing the request without the acquisition at the end of
   the hold is a state proof that it is blocked -- the only code between the
   two is the request itself, so neither a slow interpreter nor a git-heavy
   preflight can be mistaken for a lock wait (fix-r4: the old 3 s quiet-window
   inference could score a live no-lock mutant as KILLED on a cold cache).
   Delete the driver's `with tree_lock(...)` and `lock-acquired` follows the
   request at once: SURVIVED.
2. `dirty-src` — one tracked product source is dirtied *while the control holds
   the lock*, and the driver is started inside that same hold, so it is already
   blocked on the lock when the bytes change and finds them dirty the instant it
   is released. There is no window in which it could build. Delete the dirty
   gate and it builds and measures: SURVIVED. The refusal must name the dirty
   tree, so an unrelated driver failure cannot pass for a killed mutation.
   Known seam: between the release and this driver's own acquisition a sibling
   runner can win the lock and see the marker; that run reports its own
   `src/ is dirty` INVALID and must be retried. It is a false FAIL of the
   sibling, never a false PASS here.

The source is restored and byte-verified in a `finally`. The restore re-takes
the tree lock with a bounded wait (fix-r4): releasing the lock while the source
still carries the marker let a sibling runner acquire a dirty tree and record a
false INVALID. A sibling that holds the lock for longer than the bound cannot
skip the restore, so the wait falls back to the unlocked path and says so in
the complaint. The restore only writes when the bytes on disk are still exactly
what this process wrote: a file that is neither HEAD nor HEAD+marker is
somebody else's uncommitted work and is left untouched, with the run reported
as INVALID. The restore itself goes through a temp file and `os.replace`, so a
concurrent reader sees either the old bytes or the old bytes plus marker, never
a truncated source. The temp file lives in the worktree root, outside the
guarded `src/`: a `src/`-local temp that survived a kill between write and
replace would be an untracked build input and every later guard run would fail
closed on a file that is not source at all.

    CARGO_TARGET_DIR=... python3 dirty_tree_control_1555.py NEW_OUTPUT_DIR
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import _head_bytes, run_owned, tree_lock

root = Path(__file__).resolve().parents[5]
RELATIVE = 'src/pilotdeck-gateway/src/user_dialog/http_store.rs'
source = root / RELATIVE
probe = Path(__file__).resolve().parent / 'rss_probe_1555.py'
MARKER = b'\n// #1555 fix-r2 dirty-tree control\n'
HOLD_SECONDS = 10.0
# The driver must have been inside the request for at least this long when the
# lock is released; a request that lands in the last moment of the hold is a
# slow start, not a proven wait.
MIN_BLOCK_SECONDS = 2.0
LOCK_TIMEOUT = 3600
RESTORE_LOCK_TIMEOUT = 120
WATCHDOG = 1800
PROBE = [sys.executable, '-I', str(probe)]
MODE = ['--modes', 'baseline', '--rounds', '1']


def read_summary(path):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return {}


def read_log(path):
    try:
        return path.read_text()
    except OSError:
        return ''


def scratch(name):
    """Temp path for the atomic swaps: same filesystem, outside `src/`.

    `os.replace` needs one filesystem. The worktree root qualifies and is not
    covered by `_assert_workspace_sources` (`src`, `.cargo`, `rust-toolchain*`),
    so a temp file left by a killed run cannot block later guard runs.
    """
    return root / f'.negctl-{os.getpid()}-{name}'


def launch(out, env, name, log):
    """Start the driver with its own output file, in its own session."""
    return subprocess.Popen(
        PROBE + [str(out / name)] + MODE, cwd=root, env=env, stdout=log,
        stderr=subprocess.STDOUT, text=True, start_new_session=True)


def watch(process, path):
    """Return what the driver printed while the caller still holds the lock.

    `lock-requested` is printed immediately before the driver's blocking
    `flock` call and `lock-acquired` as the first statement after it returns,
    so the pair is a state proof, not a quiet-window inference: the only code
    between the two lines is the request. Seeing the request without the
    acquisition at the end of the hold means the driver is inside the request;
    it cannot have acquired the lock the control holds, unless the lock does
    not exclude -- which is SURVIVED.

    Returns `(text, requested_at)`. No grace window: a driver that is slow to
    start simply never prints the request during the hold, and the caller
    reports INVALID rather than guessing that it was blocked.
    """
    deadline = time.monotonic() + HOLD_SECONDS
    text = ''
    requested_at = None
    while True:
        text = read_log(path)
        if requested_at is None and 'lock-requested' in text:
            requested_at = time.monotonic()
        if 'lock-acquired' in text or process.poll() is not None:
            return text, requested_at
        if time.monotonic() >= deadline:
            return text, requested_at
        time.sleep(0.05)


def settle(process, log):
    """Wait for the driver after the lock is released; return its returncode."""
    try:
        process.wait(timeout=WATCHDOG)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=60)
        raise
    finally:
        log.close()
    return process.returncode


def restore(path, original):
    """Undo this process's marker only; never overwrite someone else's bytes.

    Returns `(restored, complaint)`. `restored` is False when the bytes are not
    HEAD and not HEAD+marker, i.e. another writer touched the file while the
    control held the lock: the run must not report success and must not clobber
    that work. The write is a temp file plus `os.replace`, which is atomic for a
    reader on the same filesystem.
    """
    current = path.read_bytes()
    if current == original:
        return True, ''
    if current != original + MARKER:
        return False, ('the product source no longer matches HEAD or this '
                       'control\'s marker; left untouched for inspection')
    temp = scratch('restore')
    temp.write_bytes(original)
    os.replace(temp, path)
    if path.read_bytes() == original:
        return True, ''
    return False, 'the restore did not read back as HEAD bytes'


def restore_locked(path, original):
    """Restore the marker under the tree lock, bounded, then release.

    Releasing the lock while the marker is still on disk lets a sibling runner
    acquire a dirty tree and record a false INVALID (fix-r4). A sibling that
    holds the lock for longer than the bound must not be able to skip the
    restore, so the timeout falls back to the unlocked path and says so.
    """
    try:
        with tree_lock(root, timeout=RESTORE_LOCK_TIMEOUT):
            return restore(path, original)
    except TimeoutError as error:
        restored, complaint = restore(path, original)
        return restored, (f'{type(error).__name__}: {error} '
                          f'(restored without the lock); {complaint}')


def lock_check(out, env):
    """The lock makes the driver wait; the clean run proves the driver works."""
    positive = None
    positive_error = ''
    try:
        positive = run_owned(PROBE + [str(out / 'probe-clean')] + MODE,
                             cwd=root, env=env, timeout=WATCHDOG)
    except (subprocess.TimeoutExpired, OSError) as error:
        positive_error = f'{type(error).__name__}: {error}'
    positive_summary = read_summary(out / 'probe-clean' / 'summary.json')
    positive_minima = positive_summary.get('minima') or {}
    positive_ok = (positive is not None and positive.returncode == 0
                   and positive_summary.get('status') == 'OK'
                   and isinstance(positive_minima.get('baseline'), int))
    path = out / 'probe-lock.log'
    log = path.open('w')
    process = None
    early = ''
    reason = ''
    requested_at = None
    held = 0.0
    blocked = 0.0
    try:
        with tree_lock(root, timeout=LOCK_TIMEOUT):
            started = time.monotonic()
            process = launch(out, env, 'probe-lock', log)
            early, requested_at = watch(process, path)
            released = time.monotonic()
            held = released - started
            if requested_at is not None:
                blocked = released - requested_at
        returncode = settle(process, log)
    except (subprocess.TimeoutExpired, OSError) as error:
        reason = f'{type(error).__name__}: {error}'
        if process is not None and process.poll() is None:
            process.kill()
            process.wait(timeout=60)
        log.close()
        returncode = None
    summary = read_summary(out / 'probe-lock' / 'summary.json')
    exited_early = process is not None and process.returncode is not None and not early
    status = 'KILLED'
    if 'lock-acquired' in early:
        status = 'SURVIVED'
        reason = 'the driver acquired the shared lock while the control held it'
    elif not positive_ok:
        status = 'INVALID'
        reason = (f'clean-tree positive control failed: {positive_error} '
                  f'rc={None if positive is None else positive.returncode} '
                  f'status={positive_summary.get("status")!r} '
                  f'minima={positive_summary.get("minima")!r}')
    elif 'lock-requested' not in early:
        status = 'INVALID'
        reason = (f'the driver never asked for the lock during the hold '
                  f'(early={early.strip()[:200]!r}, early_exit={exited_early})')
    elif blocked < MIN_BLOCK_SECONDS:
        status = 'INVALID'
        reason = (f'the request appeared {blocked:.2f}s before the release, '
                  f'shorter than the {MIN_BLOCK_SECONDS}s needed to prove a wait')
    elif reason:
        status = 'INVALID'
    elif returncode != 0:
        status = 'INVALID'
        reason = f'driver exit {returncode} after the release'
    elif summary.get('status') != 'OK':
        status = 'INVALID'
        reason = f'driver status {summary.get("status")!r} after the release'
    return dict(control='lock-held-during-driver', status=status,
                held_seconds=round(held, 3), blocked_seconds=round(blocked, 3),
                positive=dict(returncode=None if positive is None else positive.returncode,
                              status=positive_summary.get('status'),
                              minima=positive_summary.get('minima')),
                early_output=early.strip().splitlines()[:3],
                returncode=returncode, driver_status=summary.get('status'),
                minima=summary.get('minima'), reason=reason)


def dirty_check(out, env, original):
    """A dirty product source must be refused before any build happens."""
    path = out / 'probe-dirty.log'
    log = path.open('w')
    process = None
    early = ''
    reason = ''
    restored, complaint = True, ''
    try:
        with tree_lock(root, timeout=LOCK_TIMEOUT):
            if source.read_bytes() != original:
                raise RuntimeError('product source differs from HEAD before the dirty check')
            temp = scratch('marker')
            temp.write_bytes(original + MARKER)
            os.replace(temp, source)
            process = launch(out, env, 'probe-dirty', log)
            early, _requested_at = watch(process, path)
        returncode = settle(process, log)
    except (subprocess.TimeoutExpired, OSError, RuntimeError) as error:
        reason = f'{type(error).__name__}: {error}'
        if process is not None and process.poll() is None:
            process.kill()
            process.wait(timeout=60)
        log.close()
        returncode = None
    finally:
        restored, complaint = restore_locked(source, original)
    summary = read_summary(out / 'probe-dirty' / 'summary.json')
    failures = [str(item) for item in (summary.get('failures') or [])]
    refused = (returncode == 2
               and summary.get('status') == 'INVALID'
               and not summary.get('minima')
               and not (out / 'probe-dirty' / 'build.log').exists()
               and any(item.startswith('src/ is dirty') for item in failures))
    if not restored:
        status = 'INVALID'
        reason = complaint
    elif reason:
        status = 'INVALID'
    elif refused:
        status = 'KILLED'
    else:
        status = 'SURVIVED'
        reason = (f'rc={returncode} status={summary.get("status")!r} '
                  f'failures={failures[:1]} '
                  f'built={(out / "probe-dirty" / "build.log").exists()}')
    return dict(control='dirty-src-driver-refused', status=status,
                returncode=returncode, driver_status=summary.get('status'),
                built=(out / 'probe-dirty' / 'build.log').exists(),
                early_output=early.strip().splitlines()[:3],
                failures=summary.get('failures'), restored=restored, reason=reason)


def main():
    if len(sys.argv) != 2:
        raise SystemExit('usage: dirty_tree_control_1555.py NEW_OUTPUT_DIR')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = os.environ.copy()
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be provided by the caller')
    original = _head_bytes(root, RELATIVE, 'HEAD')
    rows = []
    restored = False
    complaint = ''
    try:
        rows.append(lock_check(out, env))
        rows.append(dirty_check(out, env, original))
    finally:
        restored, complaint = restore_locked(source, original)
    counts = {state: sum(row['status'] == state for row in rows)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    record = dict(controls=rows, counts=counts, restored=restored,
                  restore_complaint=complaint)
    (out / 'summary.json').write_text(json.dumps(record, indent=2) + '\n')
    print(json.dumps(record), flush=True)
    return 0 if restored and counts['KILLED'] == len(rows) else 1


if __name__ == '__main__':
    raise SystemExit(main())

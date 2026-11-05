#!/usr/bin/env python3
# anchor-selfcheck: unsupported run-level negative control: holds the shared
# tree lock while the peak-RSS driver starts (the driver must stay blocked), and
# dirties one tracked source byte inside that same hold (the driver must refuse).
"""#1555 fix-r2 negative control: the peak-RSS driver takes the tree lock.

Two checks, one per half of the fix:

1. `lock-held` — on a clean tree the driver must first finish `status=OK` with
   a real minimum (`positive` below, so a driver that merely refuses everything
   cannot pass this control). The control then takes the same lock and starts
   the driver: the driver prints `starting` before it asks for the lock and
   `provenance` only after it holds it, so a `starting` line with no
   `provenance` *while the control still holds the lock* is a positive proof
   that it is blocked -- not an inference from a quiet window on a fast machine.
   Delete the driver's `with tree_lock(...)` and provenance follows `starting`
   at once: SURVIVED.
2. `dirty-src` — one tracked product source is dirtied *while the control holds
   the lock*, and the driver is started inside that same hold, so it is already
   blocked on the lock when the bytes change and finds them dirty the instant it
   is released. There is no window in which it could build. Delete the dirty
   gate and it builds and measures: SURVIVED. The refusal must name the dirty
   tree, so an unrelated driver failure cannot pass for a killed mutation.

The source is restored and byte-verified in a `finally` that deliberately does
not take the lock (a sibling runner holding it must not be able to skip the
restore and leave a mutated product source behind), but it only writes when the
bytes on disk are still exactly what this process wrote: a file that is neither
HEAD nor HEAD+marker is somebody else's uncommitted work and is left untouched,
with the run reported as INVALID. The restore itself goes through a sibling
temp file and `os.replace`, so a concurrent reader sees either the old bytes or
the old bytes plus marker, never a truncated source.

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
GRACE_SECONDS = 3.0
LOCK_TIMEOUT = 3600
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


def launch(out, env, name, log):
    """Start the driver with its own output file, in its own session."""
    return subprocess.Popen(
        PROBE + [str(out / name)] + MODE, cwd=root, env=env, stdout=log,
        stderr=subprocess.STDOUT, text=True, start_new_session=True)


def watch(process, path):
    """Return what the driver printed while the caller still holds the lock.

    Decisive without depending on a fast machine: `provenance` is printed only
    after the lock is acquired, so seeing it here means the driver never waited
    (SURVIVED); `starting` is printed before the lock is requested, so seeing it
    with no provenance means the process is up and is blocked on the lock. Only
    a driver that prints neither within the hold is a startup problem.
    """
    deadline = time.monotonic() + HOLD_SECONDS
    text = ''
    while time.monotonic() < deadline:
        text = read_log(path)
        if 'provenance ' in text:
            return text
        if 'starting ' in text or process.poll() is not None:
            # Started (or already gone): give an unlocked driver a fixed grace
            # window to print its provenance before the hold is released.
            grace = time.monotonic() + GRACE_SECONDS
            while time.monotonic() < grace:
                text = read_log(path)
                if 'provenance ' in text:
                    break
                time.sleep(0.2)
            return text
        time.sleep(0.2)
    return text


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
    temp = path.with_name(path.name + '.negctl-restore')
    temp.write_bytes(original)
    os.replace(temp, path)
    if path.read_bytes() == original:
        return True, ''
    return False, 'the restore did not read back as HEAD bytes'


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
    try:
        with tree_lock(root, timeout=LOCK_TIMEOUT):
            process = launch(out, env, 'probe-lock', log)
            early = watch(process, path)
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
    if 'provenance ' in early:
        status = 'SURVIVED'
        reason = 'the driver printed provenance while the lock was held'
    elif not positive_ok:
        status = 'INVALID'
        reason = (f'clean-tree positive control failed: {positive_error} '
                  f'rc={None if positive is None else positive.returncode} '
                  f'status={positive_summary.get("status")!r} '
                  f'minima={positive_summary.get("minima")!r}')
    elif 'starting ' not in early:
        status = 'INVALID'
        reason = (f'the driver never printed `starting` during the hold '
                  f'(early={early.strip()[:200]!r}, early_exit={exited_early})')
    elif reason:
        status = 'INVALID'
    elif returncode != 0:
        status = 'INVALID'
        reason = f'driver exit {returncode} after the release'
    elif summary.get('status') != 'OK':
        status = 'INVALID'
        reason = f'driver status {summary.get("status")!r} after the release'
    return dict(control='lock-held-during-driver', status=status,
                held_seconds=HOLD_SECONDS,
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
            temp = source.with_name(source.name + '.negctl-marker')
            temp.write_bytes(original + MARKER)
            os.replace(temp, source)
            process = launch(out, env, 'probe-dirty', log)
            early = watch(process, path)
        returncode = settle(process, log)
    except (subprocess.TimeoutExpired, OSError, RuntimeError) as error:
        reason = f'{type(error).__name__}: {error}'
        if process is not None and process.poll() is None:
            process.kill()
            process.wait(timeout=60)
        log.close()
        returncode = None
    finally:
        restored, complaint = restore(source, original)
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
        restored, complaint = restore(source, original)
    counts = {state: sum(row['status'] == state for row in rows)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    record = dict(controls=rows, counts=counts, restored=restored,
                  restore_complaint=complaint)
    (out / 'summary.json').write_text(json.dumps(record, indent=2) + '\n')
    print(json.dumps(record), flush=True)
    return 0 if restored and counts['KILLED'] == len(rows) else 1


if __name__ == '__main__':
    raise SystemExit(main())

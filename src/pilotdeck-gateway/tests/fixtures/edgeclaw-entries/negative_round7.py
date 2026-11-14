#!/usr/bin/env python3
"""PR #537 round 7: strip each IO wrapper, require runtime assertion failure.

#1087 fix-r5 (sibling #3): the verdict rows stay in memory for the whole
guarded window and one final ``summary.json`` is published by a single rename
only after ``mutating_sources`` restored and byte-verified the guarded source
(the same skeleton as the edgeclaw tables). An abort inside the window — a
cancellation, a restore failure, the source guard — voids the recorded rows,
publishes one INVALID summary and exits 3; the old ``finally``-inside-the-window
write left a partial all-killed-looking summary behind when the run was
cancelled mid-suite. Every guarded-source write in the window — mutation and
restore alike — is one atomic rename (``write_source_atomic``), never a
truncating write. A stale anchor is a refusal: exit 2 before any compile,
nothing measured, nothing published.
"""
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (dead_stream_print, detach_dead_streams,
                              mutating_sources, new_run_token, refuse_summary,
                              run_owned, target_environment, unique_summary_tmp,
                              wait_for_slot, write_source_atomic)

root = Path(__file__).resolve().parents[5]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
# #1087 fix-r6 (P1a) / fix-r8 (S1-b): every evidence name carries the shared
# per-run token (pid.serial.time_ns) — the OUT_DIR above is a plain reused
# mkdir, so two overlapping runs of this table must not truncate each other's
# fixed-name artifacts, and the time_ns component survives pid recycling
# (a recycled pid must not replay another run's token).
run_token = new_run_token()
# #1087 fix-r8 (S2-a): the verdict summary is per-run, too — this run
# publishes its own file and never overwrites another run's green verdict
# recorded in the same reused OUT_DIR.
summary_name = f'summary.{run_token}.json'
path = root/'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/storage/snapshots.rs'
mutations = [
    ('lstat', 'fs::symlink_metadata(src).map_err(|e| crate::node_io::path_error(e, "lstat", src))?',
     'fs::symlink_metadata(src)?', 'round7_lstat_matches_real_ts_fallback'),
    ('readlink', 'fs::read_link(src).map_err(|e| crate::node_io::path_error(e, "readlink", src))',
     'fs::read_link(src)', 'round7_readlink_matches_native_boundary'),
    ('realpath', 'fs::canonicalize(parent).map_err(|e| crate::node_io::path_error(e, "realpath", parent))',
     'fs::canonicalize(parent)', 'round7_realpath_matches_native_boundary'),
    ('symlink', '''std::os::unix::fs::symlink(&target, dst)
            .map_err(|e| pair_error(e, "symlink", &target, dst))?''',
     'std::os::unix::fs::symlink(&target, dst)?', 'round7_symlink_matches_real_ts_readonly_destination'),
    ('chmod', 'fs::set_permissions(dst, permissions).map_err(|e| crate::node_io::path_error(e, "chmod", dst))',
     'fs::set_permissions(dst, permissions)', 'round7_chmod_matches_native_boundary'),
    ('copyfile', 'fs::copy(src, dst).map_err(|e| pair_error(e, "copyfile", src, dst))?',
     'fs::copy(src, dst)?', 'round7_copyfile_matches_real_ts_readonly_destination'),
    ('rename', 'Err(e) => Err(pair_error(e, "rename", src, dst).into()),',
     'Err(e) => Err(e.into()),', 'round7_rename_matches_real_ts_non_permission_error'),
]
env = target_environment(root)
rows = []
original = b''


def write_summary():
    # #1087 fix-r5 (sibling #3, concurrency #3): one staging name per process,
    # so two overlapping runs of the same table never share one fixed temp.
    # #1087 fix-r8 (S2-a): the publish target is this run's per-run summary
    # name and the payload names the run — the twin test_negative_publication
    # globs the per-run shape.
    tmp = unique_summary_tmp(out, summary_name)
    tmp.write_text(json.dumps({'runToken': run_token,
                               'results': rows,
                               'restoredSha256': hashlib.sha256(original).hexdigest()},
                              indent=2)+'\n')
    tmp.replace(out/summary_name)


def _refuse_summary(reason):
    """Publish this run's refusal in its own per-run file (#1087 fix-r6, P1c).

    The evidence directory is created with ``mkdir(exist_ok=True)`` so a host
    re-run can land in the same path; an exit-2 "never started" refusal is
    recorded in this run's own ``summary.<token>.json`` — one rename via the
    shared writer, carrying no verdict rows (the same rule file-memory /
    dangling-links keep with their ``_refuse_summary``). A previous run's
    verdict stays in its own file, untouched: it is never this run's
    evidence. A fresh directory simply receives its first file (fix-r9
    per-run markers; fix-r11 aligns this docstring with the four already
    rewritten).
    """
    # #1087 fix-r6 (P1c) / fix-r8 (S1-c + S2-a): the refused marker is this
    # run's own per-run summary file — one rename via the shared writer,
    # never another run's verdict, and a failing write reports through the
    # guarded print instead of a traceback (rc=1, "ran and was red").
    refuse_summary(out, reason, run_token, summary_name=summary_name)


def _void_pending(error):
    """Void the recorded rows and publish one INVALID summary (sibling #3)."""
    reason = f'runner: {type(error).__name__}: {error}'
    for row in rows:
        row['killed'] = False
        row['voided'] = reason
    for name, _old, _new, test in mutations[len(rows):]:
        rows.append({'name': name, 'test': test, 'exit': None, 'failures': None,
                     'killed': False, 'voided': reason})
    write_summary()
    # #1087 fix-r8 (S1-a): the shared guarded print — the summary is
    # published, "aborted = 3" is the only contract left to keep.
    dead_stream_print(f'voided {len(rows)} row(s): {reason}', stream='stderr')
    if isinstance(error, (OSError, ValueError)):
        # #1087 fix-r5 (concurrency #2): detach dead fds so the shutdown flush
        # cannot rewrite the exit code below to 120.
        detach_dead_streams()
    if isinstance(error, SystemExit):
        raise SystemExit(3) from error
    if not isinstance(error, Exception):
        raise
    raise SystemExit(3) from error


# Read the pristine bytes only under the same flock as every other in-place
# negative runner, and reserve the Cargo slot before exposing each mutation.
try:
    with mutating_sources(root, [path]) as snapshots:
        original = snapshots[path]
        # A stale anchor is a harness defect: refuse before any compile (exit 2,
        # nothing measured, nothing published).
        for name, old, _new, _test in mutations:
            count = original.decode().count(old)
            if count != 1:
                # #1087 fix-r8 (S1-a): the shared guarded print — a dead stderr
                # must not turn this exit-2 refusal into an in-window abort (exit 3
                # with a voided INVALID summary), an exec-time ``2>&-`` reroute, or a 120.
                dead_stream_print(f'STALE ANCHOR {name}: {count} matches',stream='stderr')
                raise SystemExit(2)
        for name, old, new, test in mutations:
            text = original.decode()
            slot = wait_for_slot(root/'scripts/cargo-slots.sh')
            # #1087 fix-r5 (sibling #3): one rename, never a truncating in-place
            # write; no `mode=` — the helper's own fallback stat covers the
            # shape (boundary #5).
            write_source_atomic(root, path, text.replace(old, new).encode())
            try:
                command = ['cargo', 'test', '--manifest-path', 'src/Cargo.toml', '--locked', '--offline',
                           '-p', 'pilotdeck-gateway', '--lib', test, '--', '--nocapture']
                run = run_owned(command, env=env, cwd=root, timeout=240)
                log = slot.stdout + slot.stderr + '$ ' + ' '.join(command) + '\n' + run.stdout + run.stderr
                (out/f'{name}.{run_token}.log').write_text(log)
                killed = (run.returncode == 101
                          and 'test result: FAILED. 0 passed; 1 failed;' in log
                          and 'could not compile' not in log)
                if killed:
                    failures = [line for line in log.splitlines() if 'panicked at' in line or 'left:' in line or 'right:' in line]
                    print(name+': detected runtime assertion; exit 101; '+'; '.join(failures), flush=True)
                else:
                    failures = []
                    print(name+': not killed; exit '+str(run.returncode), flush=True)
                rows.append({'name': name, 'test': test, 'exit': run.returncode,
                             'failures': failures, 'killed': killed})
            finally:
                # Same #1087 fix-r5 window: the in-loop restore is also one
                # rename; the guard's own exit verification covers the bytes.
                write_source_atomic(root, path, original)
except SystemExit as error:
    if error.code == 2:
        # The deliberate stale-anchor refusal: nothing was measured, so nothing
        # is published — and a reused directory's previous green summary is
        # invalidated before the refusal propagates, so it cannot stay
        # readable as this run's evidence (#1087 fix-r6, P1c).
        _refuse_summary('stale anchors: negative_round7.py is out of sync with its sources')
        raise
    _void_pending(error)
except BaseException as error:  # noqa: BLE001 - void, publish, then re-raise
    _void_pending(error)
# Reaching here means mutating_sources restored the guarded file and verified
# it against the baseline bytes. Only now may a summary exist.
write_summary()
killed = [row for row in rows if row['killed']]
print(f'summary: {len(killed)}/{len(rows)} killed', flush=True)
sys.exit(0 if rows and len(killed) == len(rows) else 1)

#!/usr/bin/env python3
"""Issue #680 negative controls: remove each dynamic ids-shape check and require
the real-TS idAudit replays to fail at runtime, then restore and require pass.

Anchors stay short and unique so a rustfmt change elsewhere cannot silently
break the mutation: the first `assert text.count(old) == 1` is the guard.
The tree lock is taken before reading `original` and held through the final
restore, so a concurrent negative runner can never capture mutated bytes.

#1087 fix-r5 (sibling #3): the verdict rows stay in memory for the whole
guarded window and one final ``summary.json`` is published by a single rename
only after ``mutating_sources`` restored and byte-verified the guarded sources
(the same skeleton as the edgeclaw tables). An abort inside the window — a
cancellation, a restore failure, the source guard — voids the recorded rows,
publishes one INVALID summary and exits 3; the old ``finally``-inside-the-window
write left a partial all-killed-looking summary behind when the run was
cancelled mid-suite. Every guarded-source write in the window — mutation and
restore alike — is one atomic rename (``write_source_atomic``), never a
truncating write. A stale anchor is a refusal: exit 2 before any compile,
nothing measured, nothing published.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (dead_stream_print, detach_dead_streams,
                              mutating_sources, new_run_token, refuse_summary,
                              run_owned, unique_summary_tmp, wait_for_slot,
                              write_source_atomic)

parser = argparse.ArgumentParser()
parser.add_argument('--out', required=True)
args = parser.parse_args()
root = Path(__file__).resolve().parents[5]
os.chdir(root)
out = Path(args.out).resolve()
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
path = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/storage/entries.rs'
file_memory = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/file_memory.rs'
mutations = [
    # `batch_ids` non-array branch: 42/{}/true must not collapse to an empty list.
    ('container-check', path,
     '        _ => Err(FileMemoryError::Type("ids.filter is not a function".into())),\n',
     '        _ => Ok(&[]),\n'),
    # `batch_ids` undefined/null branches: they must not become empty batches.
    ('container-nullish-check', path,
     '        JsValue::Undefined => Err(FileMemoryError::Type(\n'
     '            "Cannot read properties of undefined (reading \'filter\')".into(),\n'
     '        )),\n'
     '        JsValue::Null => Err(FileMemoryError::Type(\n'
     '            "Cannot read properties of null (reading \'filter\')".into(),\n'
     '        )),\n',
     '        JsValue::Undefined => Ok(&[]),\n'
     '        JsValue::Null => Ok(&[]),\n'),
    # `mutation_ids` non-string branch: 42/{}/[] must not silently become "".
    ('element-check', path,
     '            _ => Err(FileMemoryError::Type(\n'
     '                "relativePath.replace is not a function".into(),\n'
     '            )),\n',
     '            _ => Ok(JsString::from("")),\n'),
    # undefined/null elements must not be treated as empty strings.
    ('nullish-element-check', path,
     '            JsValue::Undefined => Err(FileMemoryError::Type(\n'
     '                "Cannot read properties of undefined (reading \'replace\')".into(),\n'
     '            )),\n'
     '            JsValue::Null => Err(FileMemoryError::Type(\n'
     '                "Cannot read properties of null (reading \'replace\')".into(),\n'
     '            )),\n',
     '            JsValue::Undefined => Ok(JsString::from("")),\n'
     '            JsValue::Null => Ok(JsString::from("")),\n'),
    # ["valid", 5] must reject 5 before the valid id is written.
    ('element-order-check', path,
     '        .iter()\n        .map(|value| match value {\n',
     '        .iter()\n        .take(1)\n        .map(|value| match value {\n'),
    # [null, 5] must report the first element (null), not the later number.
    ('element-reverse-check', path,
     '        .iter()\n        .map(|value| match value {\n',
     '        .iter()\n        .rev()\n        .map(|value| match value {\n'),
    # A genuinely empty array is an empty batch, not a shape error.
    ('empty-array-check', path,
     '        JsValue::Array(items) => Ok(items),\n',
     '        JsValue::Array(items) if !items.is_empty() => Ok(items),\n'),
    # getMemoryRecordsByIds filters non-string elements; it must not panic or
    # keep them as ids.
    ('get-element-filter-check', path,
     '            .filter_map(JsValue::as_string)\n            .cloned()\n',
     '            .map(|value| value.as_string().unwrap())\n            .cloned()\n'),
    # Empty batches repair MEMORY.md. Changing the repaired bytes while keeping
    # the public result must fail the strict diskAfter comparison; without that
    # check the old diskUnchanged:false boolean would still pass.
    ('repair-manifest-bytes', file_memory,
     '                self.write_bytes("MEMORY.md", next)?;\n',
     '                self.write_bytes("MEMORY.md", format!("{next}\\n"))?;\n'),
    # The added archiveTmpEntries batches must be live: the current-project
    # mode rejects before reading ids, so change that observable error text.
    ('archive-error-text', file_memory,
     '                    "archive_tmp is not supported in EdgeClaw current-project memory mode".into(),\n',
     '                    "archive_tmp unsupported".into(),\n'),
]
env = os.environ.copy()
command = ['cargo', 'test', '--manifest-path', 'src/Cargo.toml', '--locked', '--offline',
           '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_entries', '--',
           'malformed_batch_ids', '--nocapture']
rows = []
restored_pass = False
# The guard snapshot's sha256 map, published only after the window closes;
# None when the guard refused before it snapshotted anything.
restored_sha = None


def write_summary():
    # #1087 fix-r5 (sibling #3, concurrency #3): one staging name per process,
    # so two overlapping runs of the same table never share one fixed temp.
    # #1087 fix-r8 (S2-a): the publish target is this run's per-run summary
    # name and the payload names the run — the twin test_negative_publication
    # globs the per-run shape.
    tmp = unique_summary_tmp(out, summary_name)
    tmp.write_text(json.dumps({
        'runToken': run_token,
        'results': rows,
        'restoredSha256': restored_sha,
        'restoredPass': bool(restored_pass),
        # The guard itself byte-verifies the restored tree before the window
        # closes; the old manual re-read is kept so the schema stays additive.
        'restoredBytes': True,
        'complete': len(rows) == len(mutations),
    }, indent=2) + '\n')
    tmp.replace(out / summary_name)


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
    for name, _target, _old, _new in mutations[len(rows):]:
        rows.append({'name': name, 'exit': None, 'failures': None,
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


try:
    # #1087 fix-r5 (sibling #3): the snapshot name has exactly one binding
    # (the with-as capture), so anchor_selfcheck can keep reading it.
    with mutating_sources(root, [path, file_memory]) as originals:
        restored_sha = {str(target.relative_to(root)): hashlib.sha256(content).hexdigest()
                        for target, content in originals.items()}
        # A stale anchor is a harness defect: refuse before any compile (exit 2,
        # nothing measured, nothing published).
        for name, target, old, _new in mutations:
            count = originals[target].decode().count(old)
            if count != 1:
                # #1087 fix-r8 (S1-a): the shared guarded print — a dead stderr
                # must not turn this exit-2 refusal into an in-window abort (exit 3
                # with a voided INVALID summary), an exec-time ``2>&-`` reroute, or a 120.
                dead_stream_print(f'STALE ANCHOR {name}: {count} matches',stream='stderr')
                raise SystemExit(2)
        for name, target, old, new in mutations:
            text = originals[target].decode()
            slot = wait_for_slot(root / 'scripts/cargo-slots.sh')
            if slot.returncode:
                raise RuntimeError('cargo slots full')
            # #1087 fix-r5 (sibling #3): one rename, never a truncating in-place
            # write; no `mode=` — the helper's own fallback stat covers the
            # shape (boundary #5).
            write_source_atomic(root, target, text.replace(old, new).encode())
            try:
                run = run_owned(command, env=env, cwd=root, timeout=300)
            finally:
                # Same #1087 fix-r5 window: the in-loop restore is also one
                # rename; the guard's own exit verification covers the bytes.
                write_source_atomic(root, target, originals[target])
            log = slot.stdout + slot.stderr + '$ ' + ' '.join(command) + '\n' + run.stdout + run.stderr
            (out / f'{name}.{run_token}.log').write_text(log)
            # #1087 fix-r5 (simplify #2): the shared dotted spelling —
            # stricter than the bare word, same aborted-run guard.
            killed = (run.returncode != 0 and 'test result: FAILED.' in log
                      and 'could not compile' not in log)
            if killed:
                failures = [line.strip() for line in log.splitlines()
                            if 'panicked at' in line or 'left:' in line or 'right:' in line]
                print(name + ': killed at runtime; ' + '; '.join(failures[:2]), flush=True)
            else:
                failures = []
                print(name + ': not killed; exit ' + str(run.returncode), flush=True)
            rows.append({'name': name, 'exit': run.returncode,
                         'failures': failures, 'killed': killed})
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh')
        if slot.returncode:
            raise RuntimeError('cargo slots full')
        run = run_owned(command, env=env, cwd=root, timeout=300)
        log = slot.stdout + slot.stderr + '$ ' + ' '.join(command) + '\n' + run.stdout + run.stderr
        (out / f'restored.{run_token}.log').write_text(log)
        restored_pass = run.returncode == 0 and 'test result: ok. 2 passed;' in log
except SystemExit as error:
    if error.code == 2:
        # The deliberate stale-anchor refusal: nothing was measured, so nothing
        # is published — and a reused directory's previous green summary is
        # invalidated before the refusal propagates, so it cannot stay
        # readable as this run's evidence (#1087 fix-r6, P1c).
        _refuse_summary('stale anchors: negative_ids.py is out of sync with its sources')
        raise
    _void_pending(error)
except BaseException as error:  # noqa: BLE001 - void, publish, then re-raise
    _void_pending(error)
# Reaching here means mutating_sources restored the guarded files and verified
# them against the baseline bytes. Only now may a summary exist.
write_summary()

killed = [row for row in rows if row['killed']]
print(f'summary: {len(killed)}/{len(rows)} killed, restoredPass={restored_pass}',
      flush=True)
sys.exit(0 if rows and len(killed) == len(rows) and restored_pass else 1)

#!/usr/bin/env python3
# anchor-selfcheck: unsupported issue-1246 executor: ROWS is not one of anchor_selfcheck's TABLES, so `--check` (below) is the anchor preflight; this script is not wired into CI or a host manifest yet
"""#1246 negative controls: the live-row qualifier of `session_clear_removed_since`.

Three mutations, each bound to one named test that must fail at its own
issue-tagged assertion message:

- ``removed-since-ignores-live-row`` drops the "the row still exists" qualifier
  (the pre-#1246 behavior): a same-id re-capture after a completed clear reads
  as removed-since-baseline again, so the turn is silently dropped.
  Bound test: ``recapture_with_the_same_id_after_clear_is_not_marked_indexed_unpersisted``.
- ``removed-since-always-false`` makes ``SessionClears::removed_since`` answer
  false unconditionally: a clear that really deleted the listed row no longer
  discards it, so the persist resurrects the cleared session's file.
  Bound test: ``clear_while_create_note_request_in_flight_discards_persist``.
  The post-clear new-id cell (``turn_captured_after_completed_clear_...``) is
  deliberately not bound: its row never enters the removed record, so
  always-false cannot change its outcome; the discard-family cell carries the
  "the judgment is not a no-op" evidence.
- ``removed-since-closed-fails-open`` flips the `Closed` arm from "keep the
  record's answer" to "fail open": a close after the clear stops discarding and
  the cleared turn's file is written again (review P2: the third error path had
  no bound control).
  Bound test: ``clear_then_close_discards_candidates_and_fails_closed``.

Every mutation runs to completion — compile gate, bound-test run, byte restore,
restored-pass check — and only the final summary decides the exit status: a
harness failure on one mutation is recorded as INVALID, never an early abort.

Usage:
  python3 -I negative_controls.py --check
  python3 -I negative_controls.py NEW_OUTPUT_DIR [mutation ...]
"""
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from negative_panic import marker_kills_named_test, named_test_failed
from negative_process import mutating_sources, run_owned, tree_lock, wait_for_slot

root = Path(__file__).resolve().parents[5]
entries_rs = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/storage/entries.rs'
file_memory_rs = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/file_memory.rs'
command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
           '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_heartbeat_indexer']

ROWS = [
    {
        'name': 'removed-since-ignores-live-row',
        'path': entries_rs,
        'old': '            Ok(rows) => Ok(rows.is_empty()),',
        'new': '            Ok(_) => Ok(true),',
        'test': 'recapture_with_the_same_id_after_clear_is_not_marked_indexed_unpersisted',
        'assertion': '#1246 the re-captured turn is new data and must be written, not silently marked indexed',
    },
    {
        'name': 'removed-since-always-false',
        'path': file_memory_rs,
        'old': '''        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session)
            .is_some_and(|state| {
                state
                    .removed
                    .get(l0_index_id)
                    .is_some_and(|cleared_at| *cleared_at > baseline)
            })''',
        'new': '''        let _ = (session, baseline, l0_index_id);
        false''',
        'test': 'clear_then_close_discards_candidates_and_fails_closed',
        'assertion': "#917 the clear generation must discard the turn even though SQLite is closed",
    },
    {
        'name': 'removed-since-closed-fails-open',
        'path': entries_rs,
        'old': '            Err(super::StorageError::Closed) => Ok(true),',
        'new': '            Err(super::StorageError::Closed) => Ok(false),',
        'test': 'clear_then_close_discards_candidates_and_fails_closed',
        'assertion': "#917 the clear generation must discard the turn even though SQLite is closed",
    },
]

STATUSES = ('KILLED', 'SURVIVED', 'INVALID')


def write_new(path, text):
    with Path(path).open('x', encoding='utf-8') as stream:
        stream.write(text)


def run_bound(label, test, env, *, compile_only=False):
    argv = list(command)
    if compile_only:
        argv.append('--no-run')
    else:
        # Exact binding runs one named test. No -q/--quiet: the judge needs the
        # per-test status lines. No --nocapture: it would interleave stdout
        # between the name and the status word.
        argv += [test, '--', '--exact']
    slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
    write_new(label.with_suffix('.slots.log'), slot.stdout + slot.stderr)
    result = run_owned(argv, cwd=root, env=env, timeout=900)
    write_new(label.with_suffix('.log'), result.stdout + result.stderr)
    return result


def passed_bound(result, test):
    log = result.stdout + result.stderr
    return (result.returncode == 0
            and f'test {test} ... ok' in log
            and 'test result: ok. 1 passed;' in log)


def killed_bound(result, test, assertion):
    log = result.stdout + result.stderr
    return (result.returncode != 0
            and named_test_failed(log, test)
            and 'test result: FAILED' in log
            and 'could not compile' not in log
            and marker_kills_named_test(log, test, assertion, target=assertion))


def main():
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    baseline = env.get('PDRS_NEGATIVE_BASELINE', 'HEAD')
    selected = [row for row in ROWS if sys.argv[2:] == []
                or row['name'] in sys.argv[2:]]
    if not selected or (sys.argv[2:] and len(selected) != len(sys.argv[2:])):
        raise SystemExit('unknown/duplicate mutation')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    results = []

    def record(name, status, reason):
        results.append({'mutation': name, 'status': status, 'reason': reason})
        print(f'{name}: {status}: {reason}', flush=True)

    try:
        with mutating_sources(root, [row['path'] for row in selected],
                              baseline=baseline) as originals:
            for row in selected:
                original = originals[row['path']].decode()
                if original.count(row['old']) != 1:
                    raise ValueError(f'{row["name"]}: stale anchor')
                if not passed_bound(run_bound(out / f'{row["name"]}-positive',
                                              row['test'], env), row['test']):
                    record(row['name'], 'INVALID',
                           'positive control did not pass the bound test')
                    continue
                row['path'].write_text(original.replace(row['old'], row['new'], 1))
                if run_bound(out / f'{row["name"]}-compile', row['test'], env,
                             compile_only=True).returncode != 0:
                    row['path'].write_bytes(originals[row['path']])
                    record(row['name'], 'INVALID', 'mutant did not compile')
                    continue
                result = run_bound(out / f'{row["name"]}-negative',
                                   row['test'], env)
                row['path'].write_bytes(originals[row['path']])
                if row['path'].read_bytes() != originals[row['path']]:
                    raise RuntimeError(f'{row["name"]}: byte restoration failed')
                if killed_bound(result, row['test'], row['assertion']):
                    status, reason = 'KILLED', 'bound test failed at its assertion'
                elif passed_bound(result, row['test']):
                    status, reason = 'SURVIVED', 'bound test still passed'
                else:
                    status, reason = 'INVALID', ('missing bound runtime failure '
                                                 '(build error, wrong panic, or missing test)')
                if not passed_bound(run_bound(out / f'{row["name"]}-restored',
                                              row['test'], env), row['test']):
                    status, reason = 'INVALID', reason + '; restored bound test did not pass'
                record(row['name'], status, reason)
    except BaseException as error:  # noqa: BLE001 - one summary, then exit code
        for row in selected[len(results):]:
            record(row['name'], 'INVALID', f'source guard: {type(error).__name__}: {error}')
        for row in results:
            if row['status'] == 'KILLED':
                row.update(status='INVALID',
                           reason=row['reason'] + '; source guard: '
                           f'{type(error).__name__}: {error}')
    counts = {status: sum(1 for row in results if row['status'] == status)
              for status in STATUSES}
    temporary = out / 'summary.json.tmp'
    temporary.write_text(json.dumps({'controls': results, 'counts': counts},
                                    indent=2) + '\n', encoding='utf-8')
    temporary.replace(out / 'summary.json')
    print('summary: ' + ', '.join(f'{status}={counts[status]}'
                                  for status in STATUSES), flush=True)
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    if sys.argv[1:] == ['--check']:
        problems = []
        # Same tree lock the mutation runner takes: a concurrent executor may
        # hold the tree with a mutant written, and reading the anchors then
        # would report a false "anchor occurs 0 times".
        with tree_lock(root):
            for row in ROWS:
                try:
                    original = row['path'].read_text(encoding='utf-8')
                except OSError as error:
                    problems.append(f'{row["name"]}: {error}')
                    continue
                count = original.count(row['old'])
                if count != 1:
                    problems.append(f'{row["name"]}: anchor occurs {count} times, expected 1')
                    continue
                if original.count(row['new']) != 0:
                    problems.append(f'{row["name"]}: mutant text already present')
        print('\n'.join(problems) if problems
              else f'{len(ROWS)} unique anchors checked')
        raise SystemExit(int(bool(problems)))
    if not sys.argv[1:] or sys.argv[1].startswith('-'):
        raise SystemExit('usage: --check | NEW_OUTPUT_DIR [mutation ...]')
    raise SystemExit(main())

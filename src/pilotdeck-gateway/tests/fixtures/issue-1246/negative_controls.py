#!/usr/bin/env python3
"""#1246 / #1656 negative controls: clear-record discrimination in the heartbeat.

Six mutations, each bound to one named test that must fail at its own
issue-tagged assertion message:

- ``removed-since-ignores-live-row`` drops the "the row still exists" qualifier
  (the pre-#1246 behavior): a same-id re-capture after a completed clear reads
  as removed-since-baseline again, so the turn is silently dropped.
  Bound test: ``recapture_with_the_same_id_after_clear_is_not_marked_indexed_unpersisted``.
- ``removed-since-always-false`` makes ``SessionClears::removed_since`` answer
  false unconditionally: a clear that really deleted the listed row no longer
  discards it, so the persist resurrects the cleared session's file.
  Bound test: ``clear_then_close_discards_candidates_and_fails_closed``.
  The post-clear new-id cell (``turn_captured_after_completed_clear_...``) is
  deliberately not bound: its row never enters the removed record, so
  always-false cannot change its outcome; the discard-family cell carries the
  "the judgment is not a no-op" evidence.
- ``removed-since-closed-fails-open`` flips the `Closed` arm from "keep the
  record's answer" to "fail open": a close after the clear stops discarding and
  the cleared turn's file is written again (review P2: the third error path had
  no bound control).
  Bound test: ``clear_then_close_discards_candidates_and_fails_closed``.
- ``discard-still-marks-recaptured-id`` restores the unconditional
  ``processed_ids.push`` the `discarded_for_clear` guard removed (review P2,
  four roles across two families): a same-id re-capture landing between the
  discard decision and the batch ``mark_l0_indexed`` is stamped indexed without
  ever being written, and the turn is lost silently.
  Bound test: ``discarded_session_does_not_mark_the_recaptured_id_indexed``.
- ``mark-ignores-clear-record`` (#1656 gap 1) restores the unconditional batch
  mark: a session that really persisted this round is cleared (row *and* file
  deleted) and re-captured with the same id before the mark, and the batch
  UPDATE retires the never-written incarnation.
  Bound test: ``persisted_turn_recaptured_after_clear_is_not_marked_indexed``.
- ``repair-removal-reported-as-clear`` (#1656 gap 3) collapses the row-absent
  arm back to "cleared": a row ``repair_l0_sessions`` deleted is reported with
  the clearSession log line, so the two causes are indistinguishable.
  Bound test: ``row_removed_by_repair_mid_heartbeat_reports_repair_not_clear``.

Every mutation runs to completion — compile gate, bound-test run, byte restore,
restored-pass check — and only the final summary decides the exit status: a
harness failure on one mutation is recorded as INVALID, never an early abort.

The ``mutants`` table is the shape ``tests/fixtures/anchor_selfcheck.py``
understands, so CI's ``--anchors-only`` preflight checks every anchor below
against the sources (occurrence count 1); #1656 gap 2 found this file exempted
from that check while its rows were a list of dicts.

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
indexer_rs = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/heartbeat/indexer.rs'
command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
           '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_heartbeat_indexer']

# (name, path, before, after, bound test, exact runtime anchor)
mutants = [
    ('removed-since-ignores-live-row',
     entries_rs,
     '            Ok(rows) => Ok(rows.is_empty()),',
     '            Ok(_) => Ok(true),',
     'recapture_with_the_same_id_after_clear_is_not_marked_indexed_unpersisted',
     '#1246 the re-captured turn is new data and must be written, not silently marked indexed'),
    ('removed-since-always-false',
     file_memory_rs,
     '''        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session)
            .is_some_and(|state| {
                state
                    .removed
                    .get(l0_index_id)
                    .is_some_and(|cleared_at| *cleared_at > baseline)
            })''',
     '''        let _ = (session, baseline, l0_index_id);
        false''',
     'clear_then_close_discards_candidates_and_fails_closed',
     "#917 the clear generation must discard the turn even though SQLite is closed"),
    ('removed-since-closed-fails-open',
     entries_rs,
     '            Err(super::StorageError::Closed) => Ok(true),',
     '            Err(super::StorageError::Closed) => Ok(false),',
     'clear_then_close_discards_candidates_and_fails_closed',
     "#917 the clear generation must discard the turn even though SQLite is closed"),
    ('discard-still-marks-recaptured-id',
     indexer_rs,
     '        if !discarded_for_clear {',
     '        if true {',
     'discarded_session_does_not_mark_the_recaptured_id_indexed',
     '#1246 a discarded session must not stamp the re-captured id indexed'),
    ('mark-ignores-clear-record',
     indexer_rs,
     '''                let markable_ids: Vec<String> = processed_ids
                    .iter()
                    .filter(|(id, persist_generation)| {
                        !self.repository.session_clear_deleted_since(
                            &session_key,
                            *persist_generation,
                            id,
                        )
                    })
                    .map(|(id, _)| id.clone())
                    .collect();''',
     '''                let markable_ids: Vec<String> = processed_ids
                    .iter()
                    .map(|(id, _)| id.clone())
                    .collect();''',
     'persisted_turn_recaptured_after_clear_is_not_marked_indexed',
     '#1656 the batch mark must not stamp an incarnation whose file was never written'),
    ('repair-removal-reported-as-clear',
     indexer_rs,
     '''            Ok(rows) if rows.is_empty() => Ok(Some(
                if self.repository.storage.l0_removed_by_repair(l0_index_id) {
                    L0Removal::Repaired
                } else {
                    L0Removal::Cleared
                },
            )),''',
     '            Ok(rows) if rows.is_empty() => Ok(Some(L0Removal::Cleared)),',
     'row_removed_by_repair_mid_heartbeat_reports_repair_not_clear',
     '#1656 the discard reason must name the repair'),
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
    baseline = env.get('PDRS_NEGATIVE_BASELINE') or 'HEAD'
    selected = [row for row in mutants if sys.argv[2:] == []
                or row[0] in sys.argv[2:]]
    if not selected or (sys.argv[2:] and len(selected) != len(sys.argv[2:])):
        raise SystemExit('unknown/duplicate mutation')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    results = []

    def record(name, status, reason):
        results.append({'mutation': name, 'status': status, 'reason': reason})
        print(f'{name}: {status}: {reason}', flush=True)

    try:
        with mutating_sources(root, [row[1] for row in selected],
                              baseline=baseline) as originals:
            for row in selected:
                name, path, old, new, test, assertion = row
                original = originals[path].decode()
                if original.count(old) != 1:
                    raise ValueError(f'{name}: stale anchor')
                if not passed_bound(run_bound(out / f'{name}-positive',
                                              test, env), test):
                    record(name, 'INVALID',
                           'positive control did not pass the bound test')
                    continue
                path.write_text(original.replace(old, new, 1))
                if run_bound(out / f'{name}-compile', test, env,
                             compile_only=True).returncode != 0:
                    path.write_bytes(originals[path])
                    record(name, 'INVALID', 'mutant did not compile')
                    continue
                result = run_bound(out / f'{name}-negative', test, env)
                path.write_bytes(originals[path])
                if path.read_bytes() != originals[path]:
                    raise RuntimeError(f'{name}: byte restoration failed')
                if killed_bound(result, test, assertion):
                    status, reason = 'KILLED', 'bound test failed at its assertion'
                elif passed_bound(result, test):
                    status, reason = 'SURVIVED', 'bound test still passed'
                else:
                    status, reason = 'INVALID', ('missing bound runtime failure '
                                                 '(build error, wrong panic, or missing test)')
                if not passed_bound(run_bound(out / f'{name}-restored',
                                              test, env), test):
                    status, reason = 'INVALID', reason + '; restored bound test did not pass'
                record(name, status, reason)
    except BaseException as error:  # noqa: BLE001 - one summary, then exit code
        for row in selected[len(results):]:
            record(row[0], 'INVALID', f'source guard: {type(error).__name__}: {error}')
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
            for row in mutants:
                name, path, old, new = row[0], row[1], row[2], row[3]
                try:
                    original = path.read_text(encoding='utf-8')
                except OSError as error:
                    problems.append(f'{name}: {error}')
                    continue
                count = original.count(old)
                if count != 1:
                    problems.append(f'{name}: anchor occurs {count} times, expected 1')
                    continue
                if original.count(new) != 0:
                    problems.append(f'{name}: mutant text already present')
        print('\n'.join(problems) if problems
              else f'{len(mutants)} unique anchors checked')
        raise SystemExit(int(bool(problems)))
    if not sys.argv[1:] or sys.argv[1].startswith('-'):
        raise SystemExit('usage: --check | NEW_OUTPUT_DIR [mutation ...]')
    raise SystemExit(main())

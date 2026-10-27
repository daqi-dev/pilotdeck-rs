#!/usr/bin/env python3
"""Reversible product mutations for edgeclaw-heartbeat-indexer.

Every row changes one existing anchor in `heartbeat/indexer.rs`, runs the one
named integration test and restores the original bytes in `finally`. A stale
anchor aborts with status 2 instead of being reported as SURVIVED, and the
runner never stops at the first non-kill: it records every KILLED / SURVIVED /
INVALID row and exits non-zero if any row is not KILLED.

New product files are not in HEAD before the coordinator commits them, so this
script is normally run after that commit. `NEGATIVE_ONLY=a,b` narrows a sample.
cargo always uses the caller's CARGO_TARGET_DIR; without it the script exits
before touching a source file.
"""
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
indexer = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/heartbeat/indexer.rs'
out = Path(sys.argv[1]).resolve()
target = os.environ.get('CARGO_TARGET_DIR')
assert target, 'negative.py requires CARGO_TARGET_DIR from the caller'
baseline = os.environ.get('NEGATIVE_BASELINE', 'HEAD')
out.mkdir(parents=True, exist_ok=True)

# (name, path, before, after, bound test, exact runtime anchor)
cases = [
    ('duplicate-suppression',
     indexer,
     '            && !has_new_content(&messages_from_value(&recent.messages), &input.messages)',
     '            && has_new_content(&messages_from_value(&recent.messages), &input.messages)',
     'capture_duplicate_suppression_matches_ts',
     '#770 case capture-duplicate-suppression differs'),
    ('capture-during-classify-marked',
     indexer,
     '''            if !processed_ids.is_empty() {
                self.fire(HeartbeatStage::BeforeMarkL0Indexed, &session_key);
                self.repository.storage.mark_l0_indexed(&processed_ids)?;
            }''',
     '''            for pending in self
                .repository
                .storage
                .list_unindexed_l0_by_session(&session_key)?
            {
                processed_ids.push(pending.l0_index_id);
            }
            if !processed_ids.is_empty() {
                self.fire(HeartbeatStage::BeforeMarkL0Indexed, &session_key);
                self.repository.storage.mark_l0_indexed(&processed_ids)?;
            }''',
     'matrix_classify_capture_matches_ts',
     '#770 case matrix-single-classify-capture differs'),
    ('clear-session-resurrection',
     indexer,
     '                let record = target_store.upsert_candidate(&routed.candidate)?;',
     '''                if self.repository.storage.list_unindexed_l0_by_session(&session.session_key)?.is_empty() {
                    continue;
                }
                let record = target_store.upsert_candidate(&routed.candidate)?;''',
     'matrix_classify_clear_session_matches_ts',
     '#770 case matrix-single-classify-clearSession differs'),
    ('close-rejection-swallowed',
     indexer,
     '''            self.repository
                .storage
                .save_index_trace(&JsValue::Object(trace), INDEX_TRACE_LIMIT)?;''',
     '''            let _ = self
                .repository
                .storage
                .save_index_trace(&JsValue::Object(trace), INDEX_TRACE_LIMIT);''',
     'matrix_general_select_close_matches_ts',
     '#770 case matrix-general-select-close differs'),
    ('general-shortlist-empty',
     indexer,
     '        let shortlist = build_general_project_shortlist(&catalog, &routing_text);',
     '        let shortlist = build_general_project_shortlist(&[], &routing_text);',
     'general_attach_matches_ts',
     '#770 case general-attach differs'),
    ('last-indexed-key',
     indexer,
     'const LAST_INDEXED_AT_STATE_KEY: &str = "lastIndexedAt";',
     'const LAST_INDEXED_AT_STATE_KEY: &str = "lastIndexedAtMutant";',
     'single_project_cycle_matches_ts',
     '#770 case single-project differs'),
]
only = {name for name in os.environ.get('NEGATIVE_ONLY', '').split(',') if name}
unknown = only - {row[0] for row in cases}
assert not unknown, f'unknown NEGATIVE_ONLY cases: {sorted(unknown)}'

env = dict(os.environ)
env['CARGO_TARGET_DIR'] = target


class AnchorExpired(RuntimeError):
    """A mutation anchor no longer matches exactly once; never a SURVIVED row."""


def cargo_test(test):
    command = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
               '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_heartbeat_indexer']
    if test:
        command.extend([test, '--', '--exact'])
    return run_owned(command, cwd=root, env=env, timeout=600)


def assertion_failure(log, anchor):
    return ('could not compile' not in log and 'test result: FAILED.' in log
            and anchor in log)


killed, survived = [], []
with mutating_sources(root, [indexer], baseline=baseline) as originals:
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    run = cargo_test(None)
    (out / 'baseline.log').write_text(slots.stdout + slots.stderr + run.stdout + run.stderr)
    if run.returncode != 0:
        print('BASELINE FAILED: unmutated suite must pass before mutations', flush=True)
        sys.exit(1)

    for name, path, before, after, test, anchor in cases:
        if only and name not in only:
            continue
        reason = None
        try:
            source = path.read_text()
            count = source.count(before)
            if count != 1:
                raise AnchorExpired(
                    f'{name}: anchor matched {count} times (expected 1); update negative.py')
            path.write_text(source.replace(before, after))
            slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
            run = cargo_test(test)
            log = run.stdout + run.stderr
            (out / f'{name}.log').write_text(slots.stdout + slots.stderr + log)
            if run.returncode == 0:
                reason = 'bound test still passed'
            elif not assertion_failure(log, anchor):
                reason = 'did not fail at the bound assertion'
        except AnchorExpired as error:
            for original, data in originals.items():
                original.write_bytes(data)
            print(f'ANCHOR EXPIRED: {error}', flush=True)
            sys.exit(2)
        except Exception as error:
            reason = f'{type(error).__name__}: {error}'
        finally:
            for original, data in originals.items():
                original.write_bytes(data)
        if reason is None:
            killed.append(name)
            print(f'KILLED   {name}: {anchor}', flush=True)
        else:
            survived.append((name, reason))
            print(f'SURVIVED {name}: {reason}', flush=True)

    # One more unmutated run under the same tree lock gives the green half of
    # the red/green pair its own raw log after every restoration.
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    run = cargo_test(None)
    restored = slots.stdout + slots.stderr + run.stdout + run.stderr
    (out / 'restored.log').write_text(restored)
    if run.returncode != 0 or 'test result: ok.' not in restored:
        print('RESTORED FAILED: the suite does not pass after restoring the original bytes',
              flush=True)
        sys.exit(1)

print(f'KILLED   {len(killed)}: ' + ', '.join(killed))
print(f'SURVIVED {len(survived)}: ' + ', '.join(name for name, _ in survived))
if survived:
    sys.exit(1)

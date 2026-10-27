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
     '                    && !has_new_content(&messages_from_value(&recent.messages), &input.messages)',
     '                    && has_new_content(&messages_from_value(&recent.messages), &input.messages)',
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
     '#770 case matrix-single-classify-clearSession storedResults candidate source differs'),
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
     '#770 case general-attach request method/url/headers/body differs'),
    ('last-indexed-key',
     indexer,
     'const LAST_INDEXED_AT_STATE_KEY: &str = "lastIndexedAt";',
     'const LAST_INDEXED_AT_STATE_KEY: &str = "lastIndexedAtMutant";',
     'single_project_cycle_matches_ts',
     '#770 case single-project differs'),
    # P1-1: put the duplicate read and the insert back on two connection
    # acquisitions. Eight barrier-started threads then insert eight rows.
    ('capture-non-atomic',
     indexer,
     '''        let timestamp = input.timestamp.unwrap_or_else(|| (self.clock)());
        let payload = stringify_json(&JsValue::Array(
            input.messages.iter().map(MemoryMessage::to_value).collect(),
        ))
        .map_err(|error| HeartbeatError::Json(error.to_string()))?
        .ok_or_else(|| HeartbeatError::Json("messages are not serializable".into()))?;
        let timestamp_js = JsString::from(timestamp.as_str());
        let payload_js = JsString::from(payload);
        let l0_index_id = text::build_l0_index_id(
            &JsString::from(input.session_key.as_str()),
            &timestamp_js,
            &payload_js,
        );
        let record = L0SessionRecord {
            l0_index_id: l0_index_id.to_utf8().unwrap_or_default(),
            session_key: input.session_key,
            timestamp,
            messages: JsValue::Array(input.messages.iter().map(MemoryMessage::to_value).collect()),
            source: input.source.unwrap_or_else(|| self.source.clone()),
            indexed: false,
            created_at: (self.clock)(),
        };
        // The read-check-insert pair must share one connection guard: TS is
        // safe because `captureL0Session` has no `await`, but this indexer is
        // shared across threads (review P1-1).
        let inserted = self
            .repository
            .storage
            .insert_l0_session_if_new(&record, |recent| {
                recent.session_key == record.session_key
                    && !has_new_content(&messages_from_value(&recent.messages), &input.messages)
            })?;
        if !inserted {
            self.info(JsString::from(format!(
                "[clawxmemory] skip duplicate l0 capture for session={}",
                record.session_key
            )));
            return Ok(None);
        }
        Ok(Some(record))''',
     '''        let timestamp = input.timestamp.unwrap_or_else(|| (self.clock)());
        let recent = self.repository.storage.list_recent_l0(1, 0)?;
        if let Some(recent) = recent.first()
            && recent.session_key == input.session_key
            && !has_new_content(&messages_from_value(&recent.messages), &input.messages)
        {
            self.info(JsString::from(format!(
                "[clawxmemory] skip duplicate l0 capture for session={}",
                input.session_key
            )));
            return Ok(None);
        }
        let payload = stringify_json(&JsValue::Array(
            input.messages.iter().map(MemoryMessage::to_value).collect(),
        ))
        .map_err(|error| HeartbeatError::Json(error.to_string()))?
        .ok_or_else(|| HeartbeatError::Json("messages are not serializable".into()))?;
        let timestamp_js = JsString::from(timestamp.as_str());
        let payload_js = JsString::from(payload);
        let l0_index_id = text::build_l0_index_id(
            &JsString::from(input.session_key.as_str()),
            &timestamp_js,
            &payload_js,
        );
        let record = L0SessionRecord {
            l0_index_id: l0_index_id.to_utf8().unwrap_or_default(),
            session_key: input.session_key,
            timestamp,
            messages: JsValue::Array(input.messages.iter().map(MemoryMessage::to_value).collect()),
            source: input.source.unwrap_or_else(|| self.source.clone()),
            indexed: false,
            created_at: (self.clock)(),
        };
        self.repository.storage.insert_l0_session(&record)?;
        Ok(Some(record))''',
     'capture_l0_session_two_threads_is_atomic',
     '#770-P1-1 exactly one concurrent capture may insert'),
    # P1-2: take candidateName from the written record instead of the routed
    # candidate. The file read-back replaces the truncated lone surrogate.
    ('trace-candidate-name-from-record',
     indexer,
     '''                    (
                        "candidateName",
                        JsValue::String(
                            routed
                                .candidate
                                .get("name")
                                .and_then(JsValue::as_string)
                                .cloned()
                                .unwrap_or_default(),
                        ),
                    ),''',
     '''                    (
                        "candidateName",
                        JsValue::String(
                            record
                                .get("name")
                                .and_then(JsValue::as_string)
                                .cloned()
                                .unwrap_or_default(),
                        ),
                    ),''',
     'single_long_emoji_name_matches_ts',
     '#770 case single-long-emoji-name storedResults candidate source differs'),
    # P1-2: rebuild the create step's outputSummary through UTF-8/lossy
    # conversion, replacing the truncated lone surrogate with U+FFFD.
    ('trace-create-output-lossy',
     indexer,
     '''                let output_summary = match &candidate_label {
                    Some(name) => text::join_text_refs([&JsString::from("created="), name], ""),
                    None => JsString::from(format!("skipped={}", kind.as_str())),
                };''',
     '''                let output_summary = match &candidate_label {
                    Some(name) => JsString::from(format!(
                        "created={}",
                        name.to_utf8()
                            .unwrap_or_else(|_| String::from_utf16_lossy(name.units()))
                    )),
                    None => JsString::from(format!("skipped={}", kind.as_str())),
                };''',
     'single_long_emoji_name_matches_ts',
     '#770 case single-long-emoji-name differs'),
    # P1-2: rebuild the General attach route key through UTF-8, dropping the
    # truncated candidate name entirely.
    ('trace-route-key-lossy',
     indexer,
     '''                    let route_key = text::join_text_refs(
                        [
                            &JsString::from(format!("project-route-{}-", session.l0_index_id)),
                            candidate
                                .get("name")
                                .and_then(JsValue::as_string)
                                .unwrap_or(&JsString::default()),
                        ],
                        "",
                    );''',
     '''                    let route_key = JsString::from(format!(
                        "project-route-{}-{}",
                        session.l0_index_id,
                        candidate
                            .get("name")
                            .and_then(JsValue::as_string)
                            .and_then(|name| name.to_utf8().ok())
                            .unwrap_or_default()
                    ));''',
     'general_attach_long_emoji_name_matches_ts',
     '#770 case general-attach-long-emoji-name differs'),
    # P1-3: drop the batch context from the classify request. URL, order and
    # scripted responses stay identical, so only the request body (and its
    # promptDebug copy) can catch it.
    ('classify-batch-context-dropped',
     indexer,
     '''                        focus_user_turn: focus_turn,
                        batch_context_messages,
                        current_project_meta: current_project_meta.as_ref(),
                        agent_id: None,''',
     '''                        focus_user_turn: focus_turn,
                        batch_context_messages: &[],
                        current_project_meta: current_project_meta.as_ref(),
                        agent_id: None,''',
     'single_project_cycle_matches_ts',
     '#770 case single-project request method/url/headers/body differs'),
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

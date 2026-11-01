#!/usr/bin/env python3
"""Reversible product mutations for edgeclaw-heartbeat-indexer.

Every row changes one existing anchor in `heartbeat/indexer.rs`, runs the one
named integration test and restores the original bytes in `finally`. A stale
anchor aborts with status 2 instead of being reported as SURVIVED, and the
runner never stops at the first non-kill: it records every KILLED / SURVIVED /
INVALID row and exits non-zero if any row is not KILLED.

Rows are `(name, path, before, after, test, anchor)` tuples for single edits,
or `dict(name=..., changes=[(path, before, after), ...], test=...,
assertion=...)` when one mutant needs several edits applied together (the
#917 before-await hoist, the fix-r2 bump move). `changes` apply in order,
each against the text the previous one produced. Mutated files: the heartbeat
indexer, (fix-r2) the storage entries module that owns the clear generation
and (#1207) the file-memory module that owns the removed-row record.

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
entries = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/storage/entries.rs'
file_memory = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/file_memory.rs'
out = Path(sys.argv[1]).resolve()
target = os.environ.get('CARGO_TARGET_DIR')
assert target, 'negative.py requires CARGO_TARGET_DIR from the caller'
baseline = os.environ.get('NEGATIVE_BASELINE', 'HEAD')
out.mkdir(parents=True, exist_ok=True)

# #917 fix-r1: the two recheck layers (the pre-loop `session_cleared` helper
# call and the in-lock per-candidate recheck) plus the commit-segment scope
# binding, all verbatim from `process_session` / `session_cleared`. fix-r3:
# both layers discriminate per row via the removed record (row id → the
# generation of the clear that deleted it).
# The anchors below must stay byte-identical to the source or the run refuses
# to start (AnchorExpired).
BEFORE_CLASSIFY_FIRE = '            self.fire(HeartbeatStage::BeforeClassify, &session.session_key);'

PRE_LOOP_RECHECK = '''            let session_cleared =
                self.session_cleared(&session.session_key, clear_baseline, &session.l0_index_id)?;
'''

IN_LOOP_RECHECK = '''                if self.repository.session_clear_removed_since(
                    &session.session_key,
                    clear_baseline,
                    &session.l0_index_id,
                ) {
'''

HELPER_MARKER = '''        if self.repository.session_clear_generation(session_key) != clear_baseline {
            return Ok(self.repository.session_clear_removed_since(
                session_key,
                clear_baseline,
                l0_index_id,
            ));
        }
'''

# #917 fix-r3 (review P0): the mismatch arm back to the unconditional
# session-level discard — a turn captured after the completed clear is
# silently dropped again.
HELPER_UNCONDITIONAL = '''        if self.repository.session_clear_generation(session_key) != clear_baseline {
            return Ok(true);
        }
'''

# #917 fix-r3 (review P0): the in-lock recheck back to the session-level
# generation comparison, ignoring the per-row removed-id set.
IN_LOOP_SESSION_ONLY = '''                if self
                    .repository
                    .session_clear_generation(&session.session_key)
                    != clear_baseline
                {
'''

L0_ARM = '            Ok(rows) => Ok(rows.is_empty()),'

CLOSED_ARM = '            Err(StorageError::Closed) => Ok(false),'

NON_CLOSED_ARM = '            Err(error) => Err(HeartbeatError::Storage(error)),'

SCOPE_BINDING = '''                let _commit_scopes = self
                    .repository
                    .snapshot_operation_scope()
                    .map_err(HeartbeatError::File)?;
'''

# The reviewer's "check once, before the awaits" shape: the marker comparison
# runs in front of the classify await and both in-flight layers are gone.
HOISTED_RECHECK = '''            let session_cleared = self
                .repository
                .session_clear_generation(&session.session_key)
                != clear_baseline;
            self.fire(HeartbeatStage::BeforeClassify, &session.session_key);'''

# The reviewer's "only recheck on the first round" shape: the per-candidate
# recheck is gated on an empty storedResults list, so every candidate after
# the first write resurrects.
FIRST_ROUND_RECHECK = '''                if stored_results_len(trace) == 0
                    && self.repository.session_clear_removed_since(
                        &session.session_key,
                        clear_baseline,
                        &session.l0_index_id,
                    )
                {
'''

DISABLED_RECHECK = '''                if false
                    && self.repository.session_clear_removed_since(
                        &session.session_key,
                        clear_baseline,
                        &session.l0_index_id,
                    )
                {
'''

DISABLED_MARKER = '''        if false && self.repository.session_clear_generation(session_key) != clear_baseline {
            return Ok(true);
        }
'''

# #917 fix-r2 (review P1-1): the clear generation must mean "the clear
# completed". Moving the bump back to its fix-r1 position — in front of the
# clear-session seam that occupies that spot now — makes a clear that fails
# midway advance the generation anyway. fix-r3: the bump carries the removed
# ids, so the faithful rollback collects the session's ids at the old position
# (claiming the rows it is about to delete) before bumping; moving only the
# line without the collection leaves the removed set empty and no longer
# reproduces the discard (that minimal textual revert is the parked pure
# rollback mutant below).
BUMP_AFTER_DELETE = '            self.session_clears.bump(session, &removed_l0_ids);\n'

CLEAR_SESSION_GUARD = '''            if session.is_empty() {
                return Err(FileMemoryError::Invalid("sessionKey is required.".into()));
            }
'''

CLAIMED_L0_IDS = '''            let claimed_l0_ids: Vec<String> = self
                .storage
                .conn()?
                .prepare("SELECT l0_index_id FROM l0_sessions WHERE session_key = ?")?
                .query_map([session], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            self.session_clears.bump(session, &claimed_l0_ids);
'''

BUMP_BEFORE_SEAM = CLAIMED_L0_IDS

# The pure fix-r2 rollback (fix-r3, review P1-2 / coordinator): only the bump
# line moves back in front of the seam, claiming nothing. The parked clear
# then advances the generation while its rows are still present — caught by
# the parked test's seam assertion on the generation.
BUMP_LINE_ONLY = '            self.session_clears.bump(session, &[]);\n'

# #917 fix-r3 (review P2): the clear-session seam right before the L0 rows
# are selected and deleted, after the session's memory files are already
# gone. Bumping here (still claiming the rows) is the "bump after
# delete_memory_entries, before the L0 DELETE" half-fix: a clear whose DELETE
# then fails has advanced the generation with the rows still in place.
# #1207 fix-r1: the seam block lives in `clear_session_l0_tail` (method-body
# indentation) since the abort-path re-arm extraction.
BEFORE_DELETE_L0_SEAM = '''        let hook = self
            .clear_session_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(hook) = hook {
            hook(ClearSessionStage::BeforeDeleteL0, session)?;
        }
'''

BUMP_BEFORE_L0_SEAM = CLAIMED_L0_IDS + BEFORE_DELETE_L0_SEAM

# The reviewer's "recheck outside the lock" shape: the snapshot scope is
# acquired and dropped again at once, so a concurrent clearSession runs while
# the commit segment is between its recheck and its writes.
DROPPED_SCOPE = '''                let commit_scopes = self
                    .repository
                    .snapshot_operation_scope()
                    .map_err(HeartbeatError::File)?;
                drop(commit_scopes);
'''

# #1207 fix-1: the guarded segment split back into two connection
# acquisitions. A capture that starts while the clear is parked at the
# `L0IdsSelected` seam lands between the two statements, is deleted
# unrecorded, and a heartbeat that already listed it would resurrect it.
# fix-r1: the guarded segment lives in `clear_session_l0_tail` (method-body
# indentation) since the abort-path re-arm extraction.
GAP_SELECT = '''        let conn = self.storage.conn()?;
        let removed_l0_ids: Vec<String> = conn
'''

GAP_SELECT_SPLIT = '''        let removed_l0_ids: Vec<String> = self
            .storage
            .conn()?
'''

GAP_DELETE = '        conn.execute("DELETE FROM l0_sessions WHERE session_key = ?", [session])?;\n'

GAP_DELETE_SPLIT = '''        self.storage
            .conn()?
            .execute("DELETE FROM l0_sessions WHERE session_key = ?", [session])?;
'''

# #1207 fix-2: the per-row generation record back to the accumulating set
# with the session-level comparison — membership alone cannot tell when a
# row was deleted, so the stale id from the first clear reads as "cleared
# since" a baseline that predates it and the re-captured turn is silently
# dropped.
REMOVED_MAP_FIELD = '''struct SessionClearState {
    generation: u64,
    removed: std::collections::HashMap<String, u64>,
}
'''

REMOVED_SET_FIELD = '''struct SessionClearState {
    generation: u64,
    removed: std::collections::HashSet<String>,
}
'''

REMOVED_MAP_INIT = '''            .or_insert(SessionClearState {
                generation: 0,
                removed: std::collections::HashMap::new(),
            });
'''

REMOVED_SET_INIT = '''            .or_insert(SessionClearState {
                generation: 0,
                removed: std::collections::HashSet::new(),
            });
'''

REMOVED_MAP_BUMP = '''        state.generation = next;
        for id in removed {
            state.removed.insert(id.clone(), next);
        }
'''

REMOVED_SET_BUMP = '''        state.generation = next;
        state.removed.extend(removed.iter().cloned());
'''

REMOVED_MAP_SINCE = '''            .is_some_and(|state| {
                state
                    .removed
                    .get(l0_index_id)
                    .is_some_and(|cleared_at| *cleared_at > baseline)
            })
'''

REMOVED_SET_SINCE = '''            .is_some_and(|state| {
                state.generation != baseline && state.removed.contains(l0_index_id)
            })
'''

# #1207 fix-2: the baseline comparison dropped from the per-row record —
# membership alone answers, so the end-to-end re-captured turn behind the
# stale entry is dropped and its row marked indexed (silent turn loss).
REMOVED_BASELINE_DROPPED = '''            .is_some_and(|state| {
                state
                    .removed
                    .get(l0_index_id)
                    .is_some_and(|_cleared_at| true)
            })
'''

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
    # #917 fix-r1: both recheck layers removed at once (the round-1 shape,
    # now split across the helper and the in-lock recheck). The golden
    # clearSession cell catches the resurrected file and storedResults.
    dict(name='clear-resurrect-recheck-removed',
         changes=[(indexer, HELPER_MARKER, DISABLED_MARKER),
                  (indexer, L0_ARM, '            Ok(_) => Ok(false),'),
                  (indexer, IN_LOOP_RECHECK, DISABLED_RECHECK)],
         test='matrix_classify_clear_session_no_resurrection',
         assertion='#917 case matrix-single-classify-clearSession storedResults candidate source differs'),
    # #917 fix-r1 (review P0): the recheck hoisted in front of the classify
    # await and both in-flight layers neutralized — the reviewer's "check
    # once before the awaits" shape. A clear that lands while the createNote
    # request is in flight is missed and the turn is resurrected.
    dict(name='clear-resurrect-recheck-before-await',
         changes=[(indexer, BEFORE_CLASSIFY_FIRE, HOISTED_RECHECK),
                  (indexer, PRE_LOOP_RECHECK, ''),
                  (indexer, IN_LOOP_RECHECK, DISABLED_RECHECK)],
         test='clear_while_create_note_request_in_flight_discards_persist',
         assertion="#917 a clear while the createNote request is in flight must discard the turn's persist"),
    # #917 fix-r1 (review P0): the in-lock recheck only fires while
    # storedResults is still empty, so a clear that lands while a later
    # candidate's select is in flight resurrects that candidate.
    ('clear-resurrect-loop-recheck-first-round-only',
     indexer,
     IN_LOOP_RECHECK,
     FIRST_ROUND_RECHECK,
     'second_label_select_request_in_flight_clear_discards_persist',
     "#917 the first label may write before the clear; the second label's in-flight clear must discard its candidate"),
    # #917 fix-r1 (review P0): the in-lock per-candidate recheck removed
    # while the pre-loop helper stays — a clear that lands during the select
    # await (after the pre-loop check) resurrects the routed candidate.
    ('clear-resurrect-loop-recheck-removed',
     indexer,
     IN_LOOP_RECHECK,
     DISABLED_RECHECK,
     'clear_during_select_request_in_flight_discards_persist',
     '#917 a clear while the select request is in flight must discard the routed candidate'),
    # #917 fix-r3 (review P0): the pre-loop recheck's mismatch arm back to
    # the unconditional session-level discard — the heartbeat that listed a
    # turn captured after the completed clear treats it as cleared and
    # silently drops it (candidates discarded, row marked indexed).
    ('clear-resurrect-post-clear-capture-dropped',
     indexer,
     HELPER_MARKER,
     HELPER_UNCONDITIONAL,
     'turn_captured_after_completed_clear_is_not_silently_dropped',
     '#917 the turn captured after the completed clear must be indexed, not silently dropped'),
    # #917 fix-r3 (review P0): the in-lock recheck ignores the per-row
    # removed-id set and compares the session-level generation again — the
    # post-clear turn passes the pre-loop check but is discarded inside the
    # commit segment.
    ('clear-resurrect-loop-recheck-ignores-removed',
     indexer,
     IN_LOOP_RECHECK,
     IN_LOOP_SESSION_ONLY,
     'turn_captured_after_completed_clear_is_not_silently_dropped',
     '#917 the turn captured after the completed clear must be indexed, not silently dropped'),
    # #917 fix-r1 (review P0): the commit segment no longer holds the snapshot
    # operation scope across its writes, so a concurrent clearSession runs
    # while the segment is parked between its recheck and its writes.
    ('clear-resurrect-recheck-outside-lock',
     indexer,
     SCOPE_BINDING,
     DROPPED_SCOPE,
     'commit_segment_blocks_concurrent_clear_until_writes_complete',
     "#917 a concurrent clearSession must stay blocked on the commit segment's snapshot locks until its writes complete"),
    # #917 fix-r1: the fail-open arm flipped. Treating a Closed lookup as
    # cleared skips the file write in the close cells and drifts them away
    # from the TS baseline (TS writes the file, then rejects at
    # setPipelineState).
    ('clear-resurrect-recheck-fail-closed',
     indexer,
     CLOSED_ARM,
     '            Err(StorageError::Closed) => Ok(true),',
     'matrix_classify_close_matches_ts',
     '#770 case matrix-single-classify-close differs'),
    # #917 fix-r1 (review P1): a non-Closed L0 read error treated as
    # not-cleared (fail-open) resurrects the turn instead of failing the
    # session with the honest storage error. fix-r2 (review P1-2): the bound
    # assertion is the run's first error — the stats join's expect — because
    # the fail-open candidate is written first and the run then rejects at
    # `markL0Indexed` on the dropped table, so the stats assertion is never
    # reached.
    ('clear-resurrect-non-closed-fail-open',
     indexer,
     NON_CLOSED_ARM,
     '            Err(_error) => Ok(false),',
     'non_closed_l0_read_error_fails_closed',
     '#917 a non-Closed L0 read error must fail the session, not reject the run'),
    # #917 fix-r2 (review P1-1 data-loss half): the bump moved back in front
    # of the seam — collecting the session's ids there and claiming them — so
    # a clear that fails midway (the seam's injected error) has already
    # advanced the generation with the rows recorded as removed; the
    # in-flight heartbeat discards the turn and marks its still-present L0
    # row indexed — silent loss.
    dict(name='clear-resurrect-bump-before-delete',
         changes=[(entries, BUMP_AFTER_DELETE, ''),
                  (entries, CLEAR_SESSION_GUARD, CLEAR_SESSION_GUARD + BUMP_BEFORE_SEAM)],
         test='failed_clear_midway_keeps_the_turn_persistable',
         assertion='#917 a clearSession that failed midway must leave the turn persistable, not silently drop it'),
    # #917 fix-r3 (review P1-2, coordinator): the pure fix-r2 rollback —
    # only the bump line moves back in front of the seam, claiming nothing.
    # The parked clear advances the generation while its rows are still
    # present; the parked test's seam assertion on the generation goes red
    # (the fix-r2 stacked L0 flip is no longer needed to kill it).
    dict(name='clear-resurrect-parked-window-resurrect',
         changes=[(entries, BUMP_AFTER_DELETE, ''),
                  (entries, CLEAR_SESSION_GUARD, CLEAR_SESSION_GUARD + BUMP_LINE_ONLY)],
         test='clear_parked_mid_scope_does_not_resurrect_turn_started_inside_window',
         assertion='#917 the parked clear must not advance the generation before its L0 deletion completes'),
    # #917 fix-r3 (review P2): the half-fix — the bump (still claiming the
    # rows) moved to the `BeforeDeleteL0` seam, after the session's memory
    # files are removed but before the L0 DELETE. A clear that then fails has
    # advanced the generation with the rows still in place; the partial-clear
    # test's parked generation assertion goes red.
    dict(name='clear-resurrect-bump-before-l0-delete',
         changes=[(entries, BUMP_AFTER_DELETE, ''),
                  (entries, BEFORE_DELETE_L0_SEAM, BUMP_BEFORE_L0_SEAM)],
         test='clear_failing_before_l0_delete_keeps_generation_and_turn',
         assertion='#917 the clear parked before its L0 delete must not have advanced the generation'),
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
    # #1207 fix-1: the guarded segment split back into two connection
    # acquisitions. A capture that starts while the clear is parked at the
    # `L0IdsSelected` seam lands between the two statements and is deleted
    # unrecorded — the bound test's event criterion sees the insert complete
    # inside the pre-release window (fix-r1 review P1: wall clocks could
    # false-green under host load) and the row is swallowed by the DELETE.
    dict(name='clear-gap-insert-swallowed',
         changes=[(entries, GAP_SELECT, GAP_SELECT_SPLIT),
                  (entries, GAP_DELETE, GAP_DELETE_SPLIT)],
         test='concurrent_capture_cannot_land_between_l0_id_selection_and_delete',
         assertion='#1207 the capture started while the clear is parked inside the guarded segment must stay blocked on the connection guard until the clear releases it'),
    # #1207 fix-2: the per-row generation record back to the accumulating set
    # with the session-level comparison — a row whose deleting clear predates
    # the baseline reads as cleared-since it, and the re-captured turn is
    # silently dropped.
    dict(name='removed-since-accumulated',
         changes=[(file_memory, REMOVED_MAP_FIELD, REMOVED_SET_FIELD),
                  (file_memory, REMOVED_MAP_INIT, REMOVED_SET_INIT),
                  (file_memory, REMOVED_MAP_BUMP, REMOVED_SET_BUMP),
                  (file_memory, REMOVED_MAP_SINCE, REMOVED_SET_SINCE)],
         test='stale_removed_entry_after_later_clear_keeps_since_semantics',
         assertion='#1207 a row whose deleting clear predates the baseline must not read as cleared-since it'),
    # #1207 fix-2: the baseline comparison dropped from the per-row record —
    # membership alone answers, so the end-to-end re-captured turn behind the
    # stale entry is dropped and its row marked indexed (silent loss).
    dict(name='removed-since-ignores-baseline',
         changes=[(file_memory, REMOVED_MAP_SINCE, REMOVED_BASELINE_DROPPED)],
         test='recaptured_turn_survives_unrelated_later_clear',
         assertion='#1207 the re-captured turn must be persisted, not silently dropped behind the stale removed entry'),
    # #1207 fix-r1 (review P1): the re-arm dropped from the abort path — an
    # indexed row keeps its file deleted forever while its L0 row survives
    # (nothing ever re-lists an indexed row). The bound test's re-arm
    # assertion goes red.
    ('clear-abort-rearm-dropped',
     entries,
     '                    self.storage.rearm_l0_indexed_for_session(session)?;\n',
     '',
     'clear_failing_after_l0_ids_selected_rearms_indexed_rows',
     '#1207 the clear that failed with the files already removed must re-arm its still-present indexed row'),
]
only = {name for name in os.environ.get('NEGATIVE_ONLY', '').split(',') if name}


def normalized(entry):
    """(name, [(path, before, after)...], bound test, exact runtime anchor)."""
    if isinstance(entry, dict):
        return entry['name'], list(entry['changes']), entry['test'], entry['assertion']
    name, path, before, after, test, anchor = entry
    return name, [(path, before, after)], test, anchor


rows = [normalized(entry) for entry in cases]
unknown = only - {name for name, _changes, _test, _anchor in rows}
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
with mutating_sources(root, [indexer, entries, file_memory], baseline=baseline) as originals:
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    run = cargo_test(None)
    (out / 'baseline.log').write_text(slots.stdout + slots.stderr + run.stdout + run.stderr)
    if run.returncode != 0:
        print('BASELINE FAILED: unmutated suite must pass before mutations', flush=True)
        sys.exit(1)

    for name, changes, test, anchor in rows:
        if only and name not in only:
            continue
        reason = None
        try:
            texts = {}
            for index, (path, before, after) in enumerate(changes, 1):
                text = texts.get(path)
                if text is None:
                    text = path.read_text()
                count = text.count(before)
                if count != 1:
                    raise AnchorExpired(
                        f'{name} change {index}: anchor matched {count} times '
                        '(expected 1); update negative.py')
                texts[path] = text.replace(before, after)
            for path, text in texts.items():
                path.write_text(text)
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

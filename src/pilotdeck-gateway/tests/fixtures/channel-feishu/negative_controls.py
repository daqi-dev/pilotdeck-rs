#!/usr/bin/env python3
"""#1329 HEAD-only channel-feishu controls; compile each selected mutant before judging it.

OUT [mutation ...], --compile-only OUT, --check (also --check-anchors).
The shared runner owns source restoration, offline cargo, nonce receipts,
patch evidence, pending registrations and the complete result summary.

The whole `channel/feishu/` tree and this fixture directory are new
files of #1329, so the matrix cannot run until the patch has landed in HEAD
(`mutating_sources` refuses uncommitted product files); the script ships with
the patch and is executed by the coordinator afterwards on the landed tree.
"""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_suite import matrix_main

root = Path(__file__).resolve().parents[5]
fixtures = Path(__file__).resolve().parents[1]
source = root / 'src/pilotdeck-gateway/src/channel/feishu'
tests = source / 'tests.rs'
MUTATIONS = [
    # The dedupe ring holds 2000 event ids; raising the cap keeps `ev-0`
    # resident, so its re-delivery answers "deduped" and the ring pin's
    # eviction assertion is the first to notice.
    ('seen-cap-raised', source / 'mod.rs',
     'const SEEN_EVENTS_MAX: usize = 2000;',
     'const SEEN_EVENTS_MAX: usize = 2001;',
     'seen_events_ring_evicts_the_oldest_id_after_2000',
     "#1329 the evicted id is no longer reported deduped"),
    # `(raw.token as string | undefined) ?? raw.header?.token` only ignores
    # the event when the shadowing token is a non-matching *string*;
    # inverting the comparison makes matching tokens ignore every turn case
    # and the mismatching one leak its inbound dispatch — both directions
    # change cells.
    ('verify-gate-inverted', source / 'parse.rs',
     '                && token\n'
     '                    .as_string()\n'
     '                    .map(|value| String::from_utf16_lossy(value.units()))\n'
     '                    .as_deref()\n'
     '                    != Some(expected)\n',
     '                && token\n'
     '                    .as_string()\n'
     '                    .map(|value| String::from_utf16_lossy(value.units()))\n'
     '                    .as_deref()\n'
     '                    == Some(expected)\n',
     'channel_matches_real_ts', '#1329 case verify-token-mismatch matches the TS oracle'),
    # 5000 characters split as 3998+1002 (limit minus the 2-unit cursor);
    # raising the limit merges the two posts into one.
    ('chunk-limit-raised', source / 'render.rs',
     'pub const MAX_TEXT_MESSAGE_LENGTH: usize = 4000;',
     'pub const MAX_TEXT_MESSAGE_LENGTH: usize = 6000;',
     'channel_matches_real_ts', '#1329 case long-text-chunked matches the TS oracle'),
    # `liveTextLimit()` is `maxMessageLength - cursor.length`; not subtracting
    # the cursor widens every segment to 4000 units, so the long-text case's
    # two posts carry the wrong split.
    ('live-text-limit-ignores-cursor', source / 'render.rs',
     '    max.saturating_sub(DEFAULT_LIVE_REPLY_CURSOR.encode_utf16().count())\n        .max(1)',
     '    max.max(1)',
     'channel_matches_real_ts', '#1329 case long-text-chunked matches the TS oracle'),
    # 99991663 must drop the cached tenant token so the text fallback
    # re-fetches; ignoring the code leaves one token POST and a stale bearer.
    ('token-invalid-ignored', source / 'client.rs',
     '            && TOKEN_INVALID_CODES.contains(&number)',
     '            && !TOKEN_INVALID_CODES.contains(&number)',
     'token_invalid_code_drops_the_cached_token',
     '#1329 the invalid token costs exactly one extra token POST'),
    # A turn queued behind the running one must be picked up when the drainer
    # looks for its next turn; stopping that hand-off strands every queued
    # turn (the live case never reaches the turn queue — its second message
    # waits in the inbound batch and is handled by the same drainer).
    ('queue-drain-stopped', source / 'mod.rs',
     '                next = self.take_next_or_release(chat_id, &mut slot);',
     '                next = None;',
     'queued_turns_drain_after_the_active_turn',
     '#1329 the drainer runs the queued turns after the live one'),
    # Only a hot-reload keeps the route; inverting the guard clears it for
    # hot-reload (the case's url_verification falls to 404) and keeps it for
    # "done" (the stopped-route case answers 200 instead of 404).
    # Inverting the stop condition clears the slot only on a hot reload, i.e.
    # the mirror image of `pilotdeckServer.stopRunningChannel`. Both directions
    # change cells, but the oracle runs `stopped-route-falls-through-404`
    # (index 10) before `hot-reload-keeps-route` (index 12), so the stop
    # direction is the first mismatch and the one the receipt must name.
    ('route-clear-inverted', source / 'mod.rs',
     '            if reason != "hot-reload"',
     '            if reason == "hot-reload"',
     'channel_matches_real_ts', '#1329 case stopped-route-falls-through-404 matches the TS oracle'),
    # Dropping a case from the parsed oracle must trip the generator's
    # literal 13, not silently pass with fewer cells.
    ('oracle-shrunk-list', tests,
     '    let oracle = oracle();\n    let lark = Lark::new();\n',
     '    let mut oracle = oracle();\n'
     '    let _ = oracle["cases"].as_array_mut().expect("#1329 mutant cases").pop();\n'
     '    oracle["expectedCaseCount"] = json!(12);\n'
     '    let lark = Lark::new();\n',
     'channel_matches_real_ts', "#1329 oracle holds the generator's 13 cases"),
    ('oracle-wrong-declaration', fixtures / 'channel-feishu/oracle.json',
     '"expectedCaseCount": 13,',
     '"expectedCaseCount": 12,',
     'channel_matches_real_ts', '#1329 oracle case count matches its declaration'),
    # An empty cases array must be rejected, never skipped.
    ('oracle-empty-cases', fixtures / 'channel-feishu/oracle.json',
     '  "cases": [\n',
     '  "cases": [],\n  "disabledCases": [\n',
     'channel_matches_real_ts', '#1329 oracle cases non-empty'),

    # #2055 (p2batch-x concurrency close-out). The gated-turn case pins the
    # two generation gates separately: the buffered stale delta can only
    # leave through the tail flush gate, while the stale elicitation question
    # sends the moment it passes the per-event gate — one killer per gate.
    ('gated-per-event-turn-check-dropped', source / 'mod.rs',
     '                    Ok(Some(event)) => {\n'
     '                        if !self.is_current(chat_id, turn.generation) {\n'
     '                            break;\n'
     '                        }\n',
     '                    Ok(Some(event)) => {\n'
     '                        if !self.is_current(chat_id, turn.generation) && false {\n'
     '                            break;\n'
     '                        }\n',
     'gated_submit_superseded_by_new_sends_nothing_and_deletes_reaction',
     "#2055 the superseded turn's stale elicitation question is never asked"),
    # The flush-side generation gate is a pair: the tail check before the
    # final flush plus `send_final`'s per-segment narrowing (the Rust-side
    # recheck a single-threaded TS gets for free — a documented narrowing,
    # not a guarantee). Each half alone backs the other up, so the row removes
    # both; the buffered delta the case stages while the turn is current can
    # then reach the user, which is exactly the leak the pair exists to stop.
    ('gated-flush-and-segment-checks-dropped', source / 'mod.rs',
     '        if self.is_current(chat_id, turn.generation) {\n'
     '            self.interaction.elicitation_clear(chat_id);\n'
     '            self.interaction.permission_clear_after_turn(chat_id);\n'
     '            if !js_trim(&live.text).is_empty() {\n'
     '                self.send_final(chat_id, turn.generation, &live.text).await;\n'
     '            }\n'
     '        }\n'
     '        // The reaction is not part of the flush: TS removes it in a `finally`,\n'
     '        // so an aborted (superseded) turn still clears the reaction it put on\n'
     "        // the user's message. Gating this on `current` would strand it.\n"
     '        if let (Some(message_id), Some(reaction_id), Some(api)) =\n'
     '            (&turn.message_id, reaction_id, api)\n'
     '        {\n'
     '            api.remove_reaction(message_id, &reaction_id).await;\n'
     '        }\n'
     '        if self\n'
     '            .active_run(chat_id)\n'
     '            .is_some_and(|run| run.generation == turn.generation)\n'
     '        {\n'
     '            self.clear_active_run(chat_id);\n'
     '        }\n'
     '    }\n'
     '\n'
     '    // ----- outbound --------------------------------------------------------\n'
     '\n'
     '    /// `sendLiveMessage` for a final segment: the `post` payload first, the\n'
     '    /// raw `text` message as its fallback, chunked like `splitForTransport`.\n'
     '    /// The final flush of one turn: the controller\'s `append`/`flushSegment`\n'
     '    /// pair, which splits at `liveTextLimit()` and sends each segment as a\n'
     '    /// final `post`, falling back to a plain text message per segment when\n'
     '    /// the post call reports `false`.\n'
     '    async fn send_final(self: &Arc<Self>, chat_id: &str, generation: u64, text: &str) {\n'
     '        let Some(api) = self.api() else {\n'
     '            return;\n'
     '        };\n'
     '        for chunk in live_chunks(text, live_text_limit(MAX_TEXT_MESSAGE_LENGTH)) {\n'
     '            // Re-check before every segment: if the generation has already\n'
     '            // changed by the time we look, this segment does not start. This is\n'
     '            // a narrowing, not a guarantee — a bump landing between the check\n'
     '            // and the send, or while a segment is already inside `send_post`\n'
     '            // (token fetch + POST) or its `send_raw` fallback, still lets that\n'
     '            // segment land, exactly like TS\'s best-effort `abortTurn` for a\n'
     '            // send already dispatched.\n'
     '            if !self.is_current(chat_id, generation) {\n',
     '        if true {\n'
     '            self.interaction.elicitation_clear(chat_id);\n'
     '            self.interaction.permission_clear_after_turn(chat_id);\n'
     '            if !js_trim(&live.text).is_empty() {\n'
     '                self.send_final(chat_id, turn.generation, &live.text).await;\n'
     '            }\n'
     '        }\n'
     '        // The reaction is not part of the flush: TS removes it in a `finally`,\n'
     '        // so an aborted (superseded) turn still clears the reaction it put on\n'
     "        // the user's message. Gating this on `current` would strand it.\n"
     '        if let (Some(message_id), Some(reaction_id), Some(api)) =\n'
     '            (&turn.message_id, reaction_id, api)\n'
     '        {\n'
     '            api.remove_reaction(message_id, &reaction_id).await;\n'
     '        }\n'
     '        if self\n'
     '            .active_run(chat_id)\n'
     '            .is_some_and(|run| run.generation == turn.generation)\n'
     '        {\n'
     '            self.clear_active_run(chat_id);\n'
     '        }\n'
     '    }\n'
     '\n'
     '    // ----- outbound --------------------------------------------------------\n'
     '\n'
     '    /// `sendLiveMessage` for a final segment: the `post` payload first, the\n'
     '    /// raw `text` message as its fallback, chunked like `splitForTransport`.\n'
     '    /// The final flush of one turn: the controller\'s `append`/`flushSegment`\n'
     '    /// pair, which splits at `liveTextLimit()` and sends each segment as a\n'
     '    /// final `post`, falling back to a plain text message per segment when\n'
     '    /// the post call reports `false`.\n'
     '    async fn send_final(self: &Arc<Self>, chat_id: &str, generation: u64, text: &str) {\n'
     '        let Some(api) = self.api() else {\n'
     '            return;\n'
     '        };\n'
     '        for chunk in live_chunks(text, live_text_limit(MAX_TEXT_MESSAGE_LENGTH)) {\n'
     '            // Re-check before every segment: if the generation has already\n'
     '            // changed by the time we look, this segment does not start. This is\n'
     '            // a narrowing, not a guarantee — a bump landing between the check\n'
     '            // and the send, or while a segment is already inside `send_post`\n'
     '            // (token fetch + POST) or its `send_raw` fallback, still lets that\n'
     '            // segment land, exactly like TS\'s best-effort `abortTurn` for a\n'
     '            // send already dispatched.\n'
     '            if false {\n',
     'gated_submit_superseded_by_new_sends_nothing_and_deletes_reaction',
     "#2055 the superseded turn's stale events send nothing"),
    # The elicitation occupancy's lookup: dropping the swallow lets a racing
    # second delivery start a second answer task while the first is in flight
    # (#2775 fix-r4 moved the seat to the token map — the mutation keeps the
    # token logic and only drops the check).
    ('elicitation-occupancy-check-dropped', source / 'mod.rs',
     '                if state.elicitation_answering.contains_key(&chat_id) {\n'
     '                    return true;\n'
     '                }\n'
     '                let token = state.next_answering_token;\n'
     '                state.next_answering_token += 1;\n'
     '                state.elicitation_answering.insert(chat_id.clone(), token);\n',
     '                let token = state.next_answering_token;\n'
     '                state.next_answering_token += 1;\n'
     '                state.elicitation_answering.insert(chat_id.clone(), token);\n',
     'concurrent_elicitation_events_answer_once',
     '#2055 the racing second event does not start a second answer'),
    # #2775 fix-r4 (review P1-2, DS concurrency#1): the elicitation release
    # guards the stale window with its token — reverting to the unconditional
    # remove lets an old answer (whose seat a `/new` already cleared and a
    # newer answer re-took) delete the new seat, so a third event starts a
    # second answer task for the same pending entry. The permission branch
    # guards the identical window with `get == Some(&token)`.
    ('elicitation-stale-release-unconditional', source / 'mod.rs',
     '                let mut state = runtime.lock_state();\n'
     '                if state.elicitation_answering.get(&chat_id) == Some(&token) {\n'
     '                    state.elicitation_answering.remove(&chat_id);\n'
     '                }\n',
     '                runtime.lock_state().elicitation_answering.remove(&chat_id);\n',
     'stale_elicitation_release_keeps_the_new_answers_seat',
     "#2775 the stale old-answer release must not delete the new answer's seat"),
    # TS chains the permission release onto the answer with `.finally`; the
    # catch_unwind is what keeps a Rust panic on that path. A plain await
    # unwinds the task before the release, stranding the answering token.
    ('permission-answer-panic-strands-token', source / 'mod.rs',
     '            match FutureExt::catch_unwind(std::panic::AssertUnwindSafe(answer)).await {\n'
     '                Ok(Ok(())) => {}\n'
     '                Ok(Err(error)) => {\n'
     '                    runtime.log(\n'
     '                        "error",\n'
     '                        &format!("feishu: permission answer error: {error}"),\n',
     '            match Ok::<_, Box<dyn std::any::Any + Send>>(answer.await) {\n'
     '                Ok(Ok(())) => {}\n'
     '                Ok(Err(error)) => {\n'
     '                    runtime.log(\n'
     '                        "error",\n'
     '                        &format!("feishu: permission answer error: {error}"),\n',
     'panicking_permission_answer_releases_the_token',
     '#2055 a panicking permission answer still releases the token'),
    # TS `drainInboundBatch` is a try/finally: the catch_unwind is the
    # `finally` that un-claims the batch. A panic past a plain await leaves
    # `draining` stuck and the chat's later messages queue behind it forever.
    ('drain-panic-strands-batch-claim', source / 'mod.rs',
     '                if let Err(panic) =\n'
     '                    FutureExt::catch_unwind(std::panic::AssertUnwindSafe(drain)).await\n'
     '                {\n',
     '                if let Err(panic) = Ok::<_, String>(drain.await) {\n',
     'panicking_drainer_resets_batch_and_chat_recovers',
     '#2055 a panicking drainer un-claims the batch'),
    # The `/new` follow-up must carry the post-reset generation out of the
    # reset's own critical section; dropping the assignment makes it reuse the
    # pre-`/new` read and a racing reset supersedes the follow-up.
    ('new-followup-reset-return-dropped', source / 'mod.rs',
     '            generation = self.reset_chat_interaction_state(&chat_id);\n',
     '            let _ = self.reset_chat_interaction_state(&chat_id);\n',
     'new_followup_survives_a_reset_racing_in_the_state_change_window',
     '#2055 the /new follow-up submits despite the racing reset'),
]


def main():
    guarded = list(source.glob('*.rs'))
    guarded.extend(path for path in (fixtures / 'channel-feishu').rglob('*') if path.is_file())
    args = ['--check'] if sys.argv[1:] == ['--check-anchors'] else sys.argv[1:]
    return matrix_main(root, __file__, 'channel::feishu::tests::', MUTATIONS,
                       passed_count=1, child_count=0, paths=guarded,
                       exact=True, args=args)


if __name__ == '__main__':
    raise SystemExit(main())

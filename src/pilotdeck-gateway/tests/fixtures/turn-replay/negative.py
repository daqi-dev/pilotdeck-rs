#!/usr/bin/env python3
"""Mutate committed runtime sources with the shared guard; require runtime RED.

Every anchor is checked before selection. No scratch crate or alternate target
is created; CARGO_TARGET_DIR must be provided by the caller.
"""
import argparse
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

if not os.environ.get('CARGO_TARGET_DIR'):
    sys.exit('CARGO_TARGET_DIR must be supplied by the caller')

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('output', type=Path)
parser.add_argument('--only', action='append', default=[])
args = parser.parse_args()
output = args.output.resolve()
output.mkdir(parents=True, exist_ok=False)
# Every anchor is still checked when selecting a single mutation.
requested = set(args.only)
fixture = Path(__file__).resolve().parent
rust = Path(__file__).resolve().parents[4]
turn = rust / "pilotdeck-gateway/src/runtime/turn"
cleanup = rust / "pilotdeck-gateway/src/runtime/cleanup_task.rs"
oracle_path = fixture / "oracle.json"
originals = {
    "cleanup_task.rs": cleanup.read_text(),
    "replay.rs": (turn / "replay.rs").read_text(),
    "coordinator.rs": (turn / "coordinator.rs").read_text(),
    "oracle.json": oracle_path.read_text(),
}
env = dict(os.environ, CARGO_NET_OFFLINE="true", PDRS_BASELINE=os.environ["PDRS_BASELINE"])

# Shared verbatim blocks of the current sources for the #805 panic-recovery
# pins: the guarded timer/replay calls (fix round 9) and the two
# poison-tolerant shared lock helper.
START_CANCEL_GUARDED = """        // panic still surfaces at the call site like a throwing host
        // `clearTimeout`, but the guard is dropped first so the unwind
        // cannot poison the store's lock for every session.
        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {
            self.timer.cancel(session_key);
        })) {
            drop(state);
            resume_unwind(panic);
        }"""
START_CANCEL_PLAIN = START_CANCEL_GUARDED.replace(
    "        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {\n"
    "            self.timer.cancel(session_key);\n"
    "        })) {\n"
    "            drop(state);\n"
    "            resume_unwind(panic);\n"
    "        }",
    "        self.timer.cancel(session_key);",
)
CLEAR_CANCEL_GUARDED = """        // The panic surfaces at the call site like a throwing host
        // `clearTimeout`, before either removal below (#805); the guard is
        // dropped first so it cannot poison the lock.
        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {
            self.timer.cancel(session_key);
        })) {
            drop(state);
            resume_unwind(panic);
        }"""
CLEAR_CANCEL_PLAIN = CLEAR_CANCEL_GUARDED.replace(
    "        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {\n"
    "            self.timer.cancel(session_key);\n"
    "        })) {\n"
    "            drop(state);\n"
    "            resume_unwind(panic);\n"
    "        }",
    "        self.timer.cancel(session_key);",
)
RETAIN_SET_GUARDED = """        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {
            self.timer.set(
                session_key,
                Duration::from_millis(self.options.terminal_retention_ms),
                callback,
            );
        })) {
            drop(state);
            resume_unwind(panic);
        }"""
RETAIN_SET_PLAIN = RETAIN_SET_GUARDED.replace(
    "        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {\n"
    "            self.timer.set(\n"
    "                session_key,\n"
    "                Duration::from_millis(self.options.terminal_retention_ms),\n"
    "                callback,\n"
    "            );\n"
    "        })) {\n"
    "            drop(state);\n"
    "            resume_unwind(panic);\n"
    "        }",
    "        self.timer.set(\n"
    "            session_key,\n"
    "            Duration::from_millis(self.options.terminal_retention_ms),\n"
    "            callback,\n"
    "        );",
)
RETAIN_SET_SWALLOWED = RETAIN_SET_GUARDED.replace(
    "            resume_unwind(panic);", "            let _ = panic;")
DISPOSE_CANCEL_ALL_GUARDED = """        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {
            self.timer.cancel_all();
        })) {
            drop(state);
            resume_unwind(panic);
        }"""
DISPOSE_CANCEL_ALL_PLAIN = DISPOSE_CANCEL_ALL_GUARDED.replace(
    "        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {\n"
    "            self.timer.cancel_all();\n"
    "        })) {\n"
    "            drop(state);\n"
    "            resume_unwind(panic);\n"
    "        }",
    "        self.timer.cancel_all();",
)
COORDINATOR_START_GUARDED = """        let mut state = lock(&self.state);
        // The replay store may panic through its injected timer (#805): the
        // panic surfaces at the call site like a throwing TS call, but the
        // guard is dropped first so it cannot poison the composite-state
        // lock for every session.
        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {
            self.replay.start(session_key, run_id);
        })) {
            drop(state);
            resume_unwind(panic);
        }"""
COORDINATOR_START_PLAIN = COORDINATOR_START_GUARDED.replace(
    "        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {\n"
    "            self.replay.start(session_key, run_id);\n"
    "        })) {\n"
    "            drop(state);\n"
    "            resume_unwind(panic);\n"
    "        }",
    "        self.replay.start(session_key, run_id);",
)
COORDINATOR_RETAIN_GUARDED = """        // The replay store may panic through its injected timer (#805),
        // after the sink removal — the same partial state a throwing TS
        // retainTerminal leaves. The guard is dropped first so the unwind
        // cannot poison the composite-state lock for every session.
        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {
            self.replay.retain_terminal(session_key, run_id);
        })) {
            drop(state);
            resume_unwind(panic);
        }"""
COORDINATOR_RETAIN_PLAIN = COORDINATOR_RETAIN_GUARDED.replace(
    "        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {\n"
    "            self.replay.retain_terminal(session_key, run_id);\n"
    "        })) {\n"
    "            drop(state);\n"
    "            resume_unwind(panic);\n"
    "        }",
    "        self.replay.retain_terminal(session_key, run_id);",
)
COORDINATOR_DISPOSE_GUARDED = """        // The replay store may panic through its injected timer (#805), with
        // the sinks and drain tokens already cleared — the same partial state
        // a throwing TS dispose leaves. The guard is dropped first so the
        // unwind cannot poison the composite-state lock for every session.
        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {
            self.replay.dispose();
        })) {
            drop(state);
            resume_unwind(panic);
        }"""
COORDINATOR_DISPOSE_PLAIN = COORDINATOR_DISPOSE_GUARDED.replace(
    "        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {\n"
    "            self.replay.dispose();\n"
    "        })) {\n"
    "            drop(state);\n"
    "            resume_unwind(panic);\n"
    "        }",
    "        self.replay.dispose();",
)
LOCK_HELPER = """pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}"""
LOCK_HELPER_UNWRAP = LOCK_HELPER.replace(
    ".unwrap_or_else(|poisoned| poisoned.into_inner())", ".unwrap()")
DRAIN_GUARD_TOLERANT = """                // The same tolerant lock as the drain loop: a poisoned
                // composite state must not strand the queue on a stale
                // `draining` entry (#805), which would wedge every later
                // emit for the session behind a drainer that never returns.
                let mut state = lock(&self.coordinator.state);
                if owns_drain(&state, self.session_key, self.token) {
                    state.draining.remove(self.session_key);
                    state.deliveries.remove(self.session_key);
                }"""
DRAIN_GUARD_SKIPS_POISON = DRAIN_GUARD_TOLERANT.replace(
    """                let mut state = lock(&self.coordinator.state);
                if owns_drain(&state, self.session_key, self.token) {""",
    """                if let Ok(mut state) = self.coordinator.state.lock()
                    && owns_drain(&state, self.session_key, self.token)
                {""",)

# Shared verbatim blocks of the current coordinator.rs, so the mutants that
# rearrange or recreate parts of `emit`/`start` keep their anchors exact.
EMIT_LOOKUP = """            let Some(sink) = state
                .sinks
                .get(session_key)
                .and_then(|registration| registration.upgrade())
                .map(|registration| Arc::clone(&registration.sink))
            else {
                // The host dropped the registration (or never started the
                // session): TS looks a live sink up in its map, this port
                // prunes the dead weak reference on the way out.
                state.sinks.remove(session_key);
                return false;
            };"""
EMIT_LOOKUP_TRUE = EMIT_LOOKUP.replace("return false;", "return true;")
EMIT_RUN_ID_AND_RECORD = """            // `with_run_id` panics on the null/undefined edge like the TS
            // `event.runId` throw; the guard keeps that panic from poisoning
            // the composite-state lock for every other session.
            let event = match catch_unwind(AssertUnwindSafe(|| {
                self.replay.with_run_id(session_key, event)
            })) {
                Ok(event) => event,
                Err(panic) => {
                    drop(state);
                    resume_unwind(panic);
                }
            };
            #[cfg(test)]
            self.fire_emit_run_id();
            if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {
                self.replay.record(session_key, &event);
            })) {
                drop(state);
                resume_unwind(panic);
            }"""
START_INSERT_AND_RETURN = """        state
            .sinks
            .insert(session_key.clone(), Arc::downgrade(&registration));
        TurnSinkRegistration { registration }"""
# The same emit segment with the lock not yet taken: the not-atomic mutant
# moves the lookup behind it, so the `drop(state)` unwinding guards (one at
# each guard's indentation) have to go.
EMIT_RUN_ID_AND_RECORD_UNGUARDED = EMIT_RUN_ID_AND_RECORD.replace(
    "                    drop(state);\n", ""
).replace(
    "                drop(state);\n", ""
)
# Fix round 8 (codex-r8 P1 #1): the pre-fix check asked the *sink* whether
# the registration was alive; in-flight deliveries and queued entries hold
# the sink strongly, so each mutation below reinstates one of those two
# false liveness sources with a no-op sink.
EMIT_ACCEPTS_IN_FLIGHT = """            let mut sink = state
                .sinks
                .get(session_key)
                .and_then(|registration| registration.upgrade())
                .map(|registration| Arc::clone(&registration.sink));
            if sink.is_none() && state.draining.contains_key(session_key) {
                // The pre-r8 check asked the sink: an in-flight drainer
                // holds it strongly, so the event was accepted anyway.
                sink = Some(Arc::new(|_: &JsValue| {}) as Sink);
            }
            let Some(sink) = sink else {
                // The host dropped the registration (or never started the
                // session): TS looks a live sink up in its map, this port
                // prunes the dead weak reference on the way out.
                state.sinks.remove(session_key);
                return false;
            };"""
EMIT_ACCEPTS_QUEUED = EMIT_ACCEPTS_IN_FLIGHT.replace(
    "state.draining.contains_key(session_key)",
    "state.deliveries.contains_key(session_key)",
).replace(
    "an in-flight drainer\n                // holds it strongly",
    "the delivery queue\n                // holds it strongly",
)

mutants = [
    ("event-count-off-by-one", "replay.rs",
     "while replay.events.len() > self.options.event_limit",
     "while replay.events.len() >= self.options.event_limit",
     "replay::tests::default_event_limit_and_sticky_truncation_match_ts"),
    ("byte-count-utf16-units", "replay.rs",
     "    let bytes = text.len();",
     "    let bytes = text.encode_utf16().count();",
     "replay::tests::multibyte_utf8_byte_boundary_matches_ts"),
    ("byte-limit-off-by-one", "replay.rs",
     "|| replay.bytes > self.options.byte_limit",
     "|| replay.bytes >= self.options.byte_limit",
     "replay::tests::lone_surrogate_byte_count_matches_ts"),
    ("single-event-not-evicted", "replay.rs",
     "            || replay.bytes > self.options.byte_limit",
     "            || false",
     "replay::tests::single_event_over_limit_deletes_itself_matches_ts"),
    ("terminal-expiry-disabled", "replay.rs",
     "        replay.active = false;",
     "        replay.active = true;",
     "replay::tests::terminal_retention_and_expiry_match_ts"),
    ("terminal-expiry-strict-late", "replay.rs",
     "            Duration::from_millis(self.options.terminal_retention_ms),",
     "            Duration::from_millis(self.options.terminal_retention_ms + 1),",
     "replay::tests::terminal_retention_and_expiry_match_ts"),
    ("default-retention-not-30s", "replay.rs",
     "pub const DEFAULT_TERMINAL_RETENTION_MS: u64 = 30_000;",
     "pub const DEFAULT_TERMINAL_RETENTION_MS: u64 = 29_999;",
     "replay::tests::terminal_retention_and_expiry_match_ts"),
    ("terminal-run-mismatch-ignored", "replay.rs",
     "            .is_some_and(|replay| replay.run_id == *run_id)",
     "            .is_some_and(|_| true)",
     "replay::tests::terminal_mismatch_record_and_clear_match_ts"),
    ("start-keeps-truncated", "replay.rs",
     "                events: VecDeque::new(),\n                bytes: 0,\n                truncated: false,",
     "                events: VecDeque::new(),\n                bytes: 0,\n                truncated: true,",
     "replay::tests::default_event_limit_and_sticky_truncation_match_ts"),
    ("blank-run-id-considered-present", "replay.rs",
     ".is_some_and(|run_id| run_id.units().iter().any(|unit| !js_whitespace(*unit)))",
     ".is_some_and(|run_id| !run_id.is_empty())",
     "replay::tests::with_run_id_matches_ts"),
    ("emit-records-without-sink", "coordinator.rs",
     EMIT_LOOKUP + "\n" + EMIT_RUN_ID_AND_RECORD,
     EMIT_RUN_ID_AND_RECORD + "\n" + EMIT_LOOKUP,
     "coordinator::tests::retain_terminal_removes_sink_matches_ts"),
    # gate4 sibling P2 #2: emit without a started sink must be rejected.
    ("emit-without-sink-accepted", "coordinator.rs",
     EMIT_LOOKUP,
     EMIT_LOOKUP_TRUE,
     "coordinator::tests::emit_requires_started_sink_matches_ts"),
    # gate4 sibling P2 #2: record must store the event even though it never
    # delivers.
    ("record-drops-event", "replay.rs",
     "        replay.events.push_back(StoredEvent { event, bytes });",
     "        let _ = &event;",
     "coordinator::tests::record_does_not_deliver_matches_ts"),
    # gate4 sibling P2 #2: start must replace the previous sink, not keep it.
    ("start-keeps-previous-sink", "coordinator.rs",
     START_INSERT_AND_RETURN,
     "        if !state.sinks.contains_key(session_key) {\n"
     "            state\n"
     "                .sinks\n"
     "                .insert(session_key.clone(), Arc::downgrade(&registration));\n"
     "        }\n"
     "        TurnSinkRegistration { registration }",
     "coordinator::tests::start_replaces_sink_and_buffer_matches_ts"),
    # gate4 sibling P2 #2: dispose must drop the sinks along with the replays.
    ("dispose-keeps-sinks", "coordinator.rs",
     "        state.sinks.clear();",
     "        let _ = &state.sinks;",
     "coordinator::tests::dispose_and_terminal_expiry_match_ts"),
    ("emit-does-not-fill-run-id", "replay.rs",
     "        if has_run_id(event) {\n            return event.clone();\n        }",
     "        if true {\n            return event.clone();\n        }",
     "coordinator::tests::emit_fills_and_preserves_run_id_matches_ts"),
    ("same-epoch-ignores-run-id", "coordinator.rs",
     "    left.active == right.active && left.run_id == right.run_id && left.terminal == right.terminal",
     "    left.active == right.active && true && left.terminal == right.terminal",
     "coordinator::tests::same_turn_epoch_matches_real_ts_retry_loop"),
    ("expired-terminal-not-reclaimed", "replay.rs",
     "            if expired {\n                state.replays.remove(&deadline_key);\n            }",
     "            if expired {\n                let _ = &deadline_key;\n            }",
     "replay::tests::idle_terminal_expiry_reclaims_without_store_access"),
    ("same-epoch-includes-truncation", "coordinator.rs",
     "    left.active == right.active && left.run_id == right.run_id && left.terminal == right.terminal",
     "    left.active == right.active && left.run_id == right.run_id && left.terminal == right.terminal\n        && left.truncated == right.truncated",
     "coordinator::tests::same_turn_epoch_matches_real_ts_retry_loop"),
    # --- fix round 1: one control per P0/P1 plus the two gate P2 items ---
    # codex-r1 P1 #1 / glm-r1 P1 #1: replay and sink must change in one
    # critical section.
    ("coordinator-start-not-atomic", "coordinator.rs",
     COORDINATOR_START_GUARDED + "\n        #[cfg(test)]\n        self.fire_start();\n"
     + START_INSERT_AND_RETURN,
     "        self.replay.start(session_key, run_id);\n        #[cfg(test)]\n        self.fire_start();\n        let mut state = lock(&self.state);\n"
     + START_INSERT_AND_RETURN,
     "coordinator::tests::concurrent_starts_keep_replay_and_sink_in_the_same_epoch"),
    # codex-r1 P1 #2 and #4: expiry must be timer driven and must not read a
    # wall clock; the pre-fix clock sweep expired inside public calls only.
    ("terminal-timer-not-scheduled", "replay.rs",
     RETAIN_SET_GUARDED,
     RETAIN_SET_GUARDED.replace(
         "            self.timer.set(\n"
         "                session_key,\n"
         "                Duration::from_millis(self.options.terminal_retention_ms),\n"
         "                callback,\n"
         "            );\n",
         "            let _ = (&session_key, &callback);\n"),
     "replay::tests::idle_terminal_expiry_reclaims_without_store_access"),
    # codex-r1 P1 #3: retention 0 is still setTimeout(0), not a synchronous
    # delete inside retainTerminal.
    # Fix round 5 (codex-r5 P1 #3): the deletion is now unconditional so the
    # mutation also falsifies the widened retention-5/30000 legs of the
    # boundary test (the `if retention == 0` guard only reached the zero leg).
    ("retention-deleted-synchronously", "replay.rs",
     RETAIN_SET_GUARDED,
     RETAIN_SET_GUARDED + "\n        state.replays.remove(session_key);",
     "replay::tests::terminal_stays_visible_until_the_injected_timer_runs"),
    # codex-r1 P2 #5: restart must cancel the old deadline.
    ("start-does-not-cancel-terminal-timer", "replay.rs",
     "        // TS `start` calls `clearTerminal` first: the previous terminal timer\n        // must not fire against the new run. Cancelling only reaches\n        // callbacks still queued in the timer; a callback the worker already\n        // detached compares the replay identity below and does nothing.\n        // The timer is injected code without a no-panic guarantee (#805): a\n"
     + START_CANCEL_GUARDED,
     "        // TS `start` calls `clearTerminal` first: the previous terminal timer\n        // must not fire against the new run. Cancelling only reaches\n        // callbacks still queued in the timer; a callback the worker already\n        // detached compares the replay identity below and does nothing.\n        // The timer is injected code without a no-panic guarantee (#805): a\n"
     + START_CANCEL_GUARDED.replace(
         "            self.timer.cancel(session_key);\n", "            let _ = session_key;\n"),
     "replay::tests::terminal_timer_metadata_does_not_accumulate"),
    # gate1 concurrency P2 #3: serialization happens outside the state lock, so
    # a non-serializable event cannot poison every session.
    ("record-holds-lock-across-serialization", "replay.rs",
     "        let (event, bytes) = clone_event_with_bytes(event);\n        #[cfg(test)]\n        self.fire_record_gap();\n        let mut state = lock(&self.state);",
     "        let mut state = lock(&self.state);\n        let (event, bytes) = clone_event_with_bytes(event);\n        #[cfg(test)]\n        self.fire_record_gap();",
     "replay::tests::non_serializable_record_does_not_poison_the_store"),
    # codex-r1 P1 #4: the production timer must honour the elapsed delay it was
    # given; stretching it 1000x keeps the terminal replay past the 5 s bound.
    ("production-timer-stretches-delay", "replay.rs",
     "                due: Instant::now() + delay,",
     "                due: Instant::now() + delay * 1_000,",
     "replay::tests::production_timer_fires_at_the_scheduled_delay"),
    # gate1 sibling P2 #1: the provenance guard must reject a tampered hash.
    ("tampered-source-hash", "oracle.json",
     "ff3f8206be88afac0336f0aede66c084ce40eef5ea6318d1e751c3902bc135ce",
     "0" * 64,
     "replay::tests::with_run_id_matches_ts"),
    # --- fix round 2: one control per P1 plus the coordinator poison pair ---
    # codex-r2 P1 #1: a detached callback must not delete a restarted run's
    # terminal replay; dropping the identity/serial check reintroduces it.
    ("stale-timer-deletes-restart", "replay.rs",
     "            let expired = state.replays.get(&deadline_key).is_some_and(|replay| {\n                !replay.active && replay.identity == identity && replay.retain_serial == serial\n            });",
     "            let expired = state.replays.get(&deadline_key).is_some_and(|replay| {\n                !replay.active\n            });",
     "replay::tests::stale_timer_callback_cannot_delete_a_restarted_terminal_replay"),
    # codex-r2 P1 #5: cloning options must not share one scheduler between
    # stores; eagerly constructing one in `new()` restores the shared timer.
    ("options-share-timers", "replay.rs",
     "            timer: None,",
     "            timer: Some(Arc::new(ThreadTimer::new())),",
     "replay::tests::cloned_options_do_not_share_terminal_timers_same_key"),
    # codex-r2 P1 #4: a zero-delay expiry must still fire on the production
    # timer thread; stretching the floor past the test bound wedges it.
    # (gate4 sibling P2 #3 moved the Node delay coercion to
    # pilotdeck_protocol::timer_delay_ms, so the floor mutation now raises
    # every clamped delay instead of editing the removed local branch.)
    ("zero-delay-floor-removed", "replay.rs",
     "        let delay = Duration::from_millis(timer_delay_ms(delay.as_millis() as f64));",
     "        let delay = Duration::from_millis(timer_delay_ms(delay.as_millis() as f64).max(60_000));",
     "replay::tests::production_zero_retention_reclaims_without_store_access"),
    # codex-r2 P1 #2: a drainer invalidated by dispose/restart must stop;
    # ignoring the ownership token lets it re-enter the new sink.
    ("drain-ownership-ignored", "coordinator.rs",
     "fn owns_drain(state: &CoordinatorState, session_key: &JsString, token: u64) -> bool {\n    state.draining.get(session_key) == Some(&token)\n}",
     "fn owns_drain(state: &CoordinatorState, session_key: &JsString, token: u64) -> bool {\n    let _ = (state, session_key, token);\n    true\n}",
     "coordinator::tests::disposed_drainer_cannot_touch_the_restarted_delivery_queue"),
    # codex-r2 P1 #3: a reentrant emit must deliver inside the nested call;
    # missing the drain stack defers the inner delivery past the boundary.
    ("reentrancy-undetected", "coordinator.rs",
     "                .find(|(id, key, _)| *id == self.id && key == session_key)",
     "                .find(|(_, _, _)| false)",
     "coordinator::tests::reentrant_emit_completes_before_the_outer_sink_returns_matches_ts"),
    # codex-r2 P2 #6 / gate2 boundary P2a: serializing under the coordinator
    # lock poisons it for every session.
    ("coordinator-record-poisons", "coordinator.rs",
     "        let state = lock(&self.state);\n        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {\n            self.replay.record(session_key, event);\n        })) {\n            drop(state);\n            resume_unwind(panic);\n        }",
     "        let _state = lock(&self.state);\n        self.replay.record(session_key, event);",
     "coordinator::tests::non_serializable_events_through_the_coordinator_do_not_poison_other_sessions"),
    # Fix round 7: the old `coordinator-emit-poisons` mutant (unguarded
    # record inside emit) became unfalsifiable — withRunId now panics on the
    # undefined/null event at the TS `event.runId` read, before record, so
    # no event can reach record-in-emit and fail serialization (only a
    # top-level undefined does, and only there). The poison-carrying guard
    # in emit is now the withRunId one; this mutant removes it instead.
    ("coordinator-emit-run-id-poisons", "coordinator.rs",
     "            let event = match catch_unwind(AssertUnwindSafe(|| {\n                self.replay.with_run_id(session_key, event)\n            })) {\n                Ok(event) => event,\n                Err(panic) => {\n                    drop(state);\n                    resume_unwind(panic);\n                }\n            };\n            #[cfg(test)]\n            self.fire_emit_run_id();",
     "            let event = self.replay.with_run_id(session_key, event);\n            #[cfg(test)]\n            self.fire_emit_run_id();",
     "coordinator::tests::non_serializable_events_through_the_coordinator_do_not_poison_other_sessions"),
    # gate2 concurrency P2 #2: a record serialized across a start must not be
    # adopted by the replacement run; dropping the identity re-check appends it.
    ("record-adopts-across-start", "replay.rs",
     "        if replay.identity != identity {",
     "        if false {",
     "replay::tests::record_serialized_across_a_start_is_not_adopted_by_the_new_run"),
    # gate2 sibling P2 #1: emit/retain/dispose must each stay one composite
    # critical section; splitting them reopens the codex-r1 P1 #1 class.
    ("coordinator-emit-not-atomic", "coordinator.rs",
     "            let mut state = lock(&self.state);\n"
     "            // Registration liveness, not sink liveness (codex-r8 P1 #1): the\n"
     "            // queue and any in-flight drainer hold the sink strongly, so\n"
     "            // asking the sink would accept new events after the last\n"
     "            // registration dropped.\n"
     + EMIT_LOOKUP + "\n"
     + EMIT_RUN_ID_AND_RECORD,
     "            // Registration liveness, not sink liveness (codex-r8 P1 #1): the\n"
     "            // queue and any in-flight drainer hold the sink strongly, so\n"
     "            // asking the sink would accept new events after the last\n"
     "            // registration dropped.\n"
     + EMIT_RUN_ID_AND_RECORD_UNGUARDED + "\n"
     "            let mut state = lock(&self.state);\n"
     + EMIT_LOOKUP,
     "coordinator::tests::concurrent_start_cannot_split_emit_run_id_from_its_record"),
    ("coordinator-retain-not-atomic", "coordinator.rs",
     "        let mut state = lock(&self.state);\n        state.sinks.remove(session_key);\n        #[cfg(test)]\n        self.fire_retain_remove();\n"
     + COORDINATOR_RETAIN_GUARDED,
     "        {\n            let mut state = lock(&self.state);\n            state.sinks.remove(session_key);\n        }\n        #[cfg(test)]\n        self.fire_retain_remove();\n        self.replay.retain_terminal(session_key, run_id);",
     "coordinator::tests::concurrent_retain_waits_for_the_composite_state_update"),
    ("coordinator-dispose-not-atomic", "coordinator.rs",
     "        let mut state = lock(&self.state);\n        state.sinks.clear();\n        #[cfg(test)]\n        self.fire_dispose_clear();\n        // Dropping the drain tokens makes an in-flight drainer exit before it\n        // touches a restarted session's queue; its cleanup guard is a no-op.\n        state.deliveries.clear();\n        state.draining.clear();\n"
     + COORDINATOR_DISPOSE_GUARDED,
     "        {\n            let mut state = lock(&self.state);\n            state.sinks.clear();\n        }\n        #[cfg(test)]\n        self.fire_dispose_clear();\n        let mut state = lock(&self.state);\n        state.deliveries.clear();\n        state.draining.clear();\n        self.replay.dispose();",
     "coordinator::tests::concurrent_dispose_waits_and_leaves_a_consistent_start"),
    # --- fix round 3: one control per P1/P2 plus the sibling constant gaps ---
    # codex-r3 P1 #1: a reentrant emit must queue behind the events concurrent
    # emitters already recorded; restoring the old direct sink call jumps them.
    ("reentrant-emit-jumps-queue", "coordinator.rs",
     "            match marker_token.filter(|&token| owns_drain(&state, session_key, token)) {\n                Some(token) => (Some(token), Some(sequence)),\n                None => (claim_drain(&mut state, session_key), None),\n            }",
     "            if let Some(token) = marker_token.filter(|&token| owns_drain(&state, session_key, token)) {\n                let (sink, event, _sequence) = state\n                    .deliveries\n                    .get_mut(session_key)\n                    .and_then(|queue| queue.pop_back())\n                    .expect(\"the event just pushed\");\n                drop(state);\n                let _marker = DrainMarker::push(self.id, session_key, token);\n                sink(&event);\n                return true;\n            }\n            (claim_drain(&mut state, session_key), None)",
     "coordinator::tests::reentrant_emit_waits_for_concurrently_queued_events"),
    # codex-r3 P1 #2 / grok-r3 P1 #1: a stale drain marker must be validated
    # against the current token; matching on the marker alone (the old
    # behavior) calls a restarted session's sink directly.
    ("stale-reentry-direct-call", "coordinator.rs",
     "            match marker_token.filter(|&token| owns_drain(&state, session_key, token)) {\n                Some(token) => (Some(token), Some(sequence)),\n                None => (claim_drain(&mut state, session_key), None),\n            }",
     "            if marker_token.is_some() {\n                let (sink, event, _sequence) = state\n                    .deliveries\n                    .get_mut(session_key)\n                    .and_then(|queue| queue.pop_back())\n                    .expect(\"the event just pushed\");\n                drop(state);\n                let _marker = DrainMarker::push(self.id, session_key, marker_token.unwrap_or(0));\n                sink(&event);\n                return true;\n            }\n            (claim_drain(&mut state, session_key), None)",
     "coordinator::tests::stale_reentrant_marker_goes_through_the_restarted_queue"),
    # codex-r3 P2 #4: a panicking nested delivery must leave the queue to the
    # outer drainer; arming the nested guard drops the events behind it.
    ("nested-panic-drops-queue", "coordinator.rs",
     "            armed: boundary.is_none(),",
     "            armed: true,",
     "coordinator::tests::nested_delivery_panic_keeps_the_queue_for_the_outer_drainer"),
    # gate3 sibling P2 #1: the three TS limits and their clamps had no
    # dedicated mutants; each positive control must be provably falsifiable.
    ("default-event-limit-off-by-one", "replay.rs",
     "pub const DEFAULT_EVENT_LIMIT: usize = 500;",
     "pub const DEFAULT_EVENT_LIMIT: usize = 501;",
     "replay::tests::defaults_are_the_three_ts_limits"),
    ("default-byte-limit-off-by-one", "replay.rs",
     "pub const DEFAULT_BYTE_LIMIT: usize = 256 * 1024;",
     "pub const DEFAULT_BYTE_LIMIT: usize = 256 * 1024 + 1;",
     "replay::tests::defaults_are_the_three_ts_limits"),
    ("event-limit-clamp-removed", "replay.rs",
     "        self.event_limit = value.max(1);",
     "        self.event_limit = value;",
     "replay::tests::limits_are_clamped_like_ts"),
    ("byte-limit-clamp-removed", "replay.rs",
     "        self.byte_limit = value.max(1);",
     "        self.byte_limit = value;",
     "replay::tests::limits_are_clamped_like_ts"),
    ("huge-delay-clamp-removed", "replay.rs",
     "        let delay = Duration::from_millis(timer_delay_ms(delay.as_millis() as f64));",
     "        let delay = delay;",
     "replay::tests::huge_terminal_retention_is_clamped_like_node_settimeout"),
    ("snapshot-missing-active-flipped", "replay.rs",
     "            return ActiveTurnSnapshot {\n                active: false,",
     "            return ActiveTurnSnapshot {\n                active: true,",
     "replay::tests::snapshot_without_replay_matches_ts"),
    # --- fix round 4: nested-drain boundary, queue ownership, the inner-sink
    # failure golden, the busy-gap divergence check, and the busy-gap floor ---
    # codex-r4 P1 #1: a nested drain must return once its own event's delivery
    # completes; removing the boundary makes it keep draining the tail.
    ("nested-drain-ignores-boundary", "coordinator.rs",
     "                        if boundary.is_some_and(|boundary| sequence > boundary) {",
     "                        if false {",
     "coordinator::tests::nested_drain_returns_at_the_inner_call_boundary_not_the_tail"),
    # grok-r4 P2 #1: a nested drain that empties the queue must not hand it
    # back while the outer drainer is still inside its sink call.
    ("nested-drain-releases-queue", "coordinator.rs",
     "                        if boundary.is_none() {",
     "                        if true {",
     "coordinator::tests::nested_drain_does_not_release_the_outer_drainers_queue"),
    # codex-r4 P2 #3: the inner sink's own failure must surface at the nested
    # `emit` call site; swallowing it there breaks the marker golden.
    ("nested-sink-panic-swallowed", "coordinator.rs",
     "                sink(&event);",
     "                let _ = catch_unwind(AssertUnwindSafe(|| sink(&event)));",
     "coordinator::tests::reentrant_inner_sink_failure_surfaces_at_the_inner_call_site_matches_ts"),
    # gate4 sibling P2 #2: the busy-gap divergence check must be falsifiable
    # too — the same zero-delay floor mutation, aimed at its own test.
    ("zero-delay-floor-busy-gap", "replay.rs",
     "        let delay = Duration::from_millis(timer_delay_ms(delay.as_millis() as f64));",
     "        let delay = Duration::from_millis(timer_delay_ms(delay.as_millis() as f64).max(60_000));",
     "replay::tests::production_retention_reclaims_eventually_after_a_busy_gap"),
    # --- fix round 6: one control per new A7.2-COORDINATOR-DELIVERY-001 pin
    # (gate6 sibling P1): the coordinator's disclosed port-only delivery edges
    # must be pinned by falsifiable tests, not just comments. ---
    # (a) retain_terminal must leave the in-flight drainer alone: the
    # already-accepted event still reaches the removed sink.
    ("retain-terminal-invalidates-drain", "coordinator.rs",
     "        let mut state = lock(&self.state);\n        state.sinks.remove(session_key);",
     "        let mut state = lock(&self.state);\n        state.sinks.remove(session_key);\n        state.draining.clear();",
     "coordinator::tests::accepted_events_reach_the_old_sink_after_retain_terminal_returns"),
    # (a) same for a replacing start: invalidating the drain drops e2 instead
    # of delivering it through the replaced sink.
    ("replacing-start-invalidates-drain", "coordinator.rs",
     START_INSERT_AND_RETURN,
     "        state\n"
     "            .sinks\n"
     "            .insert(session_key.clone(), Arc::downgrade(&registration));\n"
     "        state.draining.clear();\n"
     "        TurnSinkRegistration { registration }",
     "coordinator::tests::accepted_events_reach_the_old_sink_after_a_replacing_start_returns"),
    # (b) dispose must drop the accepted-but-undelivered queue; keeping it lets
    # the in-flight drainer deliver e2 past the dispose.
    ("dispose-keeps-delivery-queue", "coordinator.rs",
     "        state.deliveries.clear();\n        state.draining.clear();\n" + COORDINATOR_DISPOSE_GUARDED,
     COORDINATOR_DISPOSE_GUARDED,
     "coordinator::tests::dispose_drops_events_accepted_but_not_yet_delivered"),
    # (b) a panicking drainer's cleanup must drop the events queued behind it;
    # keeping them resurrects e2 through the next emit.
    ("panic-guard-keeps-queued-events", "coordinator.rs",
     "                    state.draining.remove(self.session_key);\n                    state.deliveries.remove(self.session_key);",
     "                    state.draining.remove(self.session_key);",
     "coordinator::tests::sink_panic_drops_events_queued_behind_it"),
    # --- fix round 7: gate7 P1 #1 (sink ownership) and P1 #2 (non-object
    # withRunId spread); each new pin must be falsifiable. ---
    # gate7 P1 #2: non-object events must spread like TS `{ ...event, runId }`;
    # returning them unchanged restores the pre-fix behavior.
    ("with-run-id-non-object-unchanged", "replay.rs",
     "        let mut object = match event {\n            JsValue::Object(object) => object.clone(),",
     "        if !matches!(event, JsValue::Object(_)) {\n            return event.clone();\n        }\n        let mut object = match event {\n            JsValue::Object(object) => object.clone(),",
     "replay::tests::with_run_id_spreads_non_object_events_like_ts"),
    # gate7 P1 #1: a strong coordinator->sink edge would recreate the
    # reference cycle; parking a strong clone in the delivery queue keeps the
    # strongly capturing sink (and through it the coordinator) alive.
    ("start-retains-strong-sink", "coordinator.rs",
     START_INSERT_AND_RETURN,
     "        state\n"
     "            .sinks\n"
     "            .insert(session_key.clone(), Arc::downgrade(&registration));\n"
     "        state\n"
     "            .deliveries\n"
     "            .entry(JsString::from(\"mutation-leak\"))\n"
     "            .or_default()\n"
     "            .push_back((Arc::clone(&registration.sink), JsValue::Null, 0));\n"
     "        TurnSinkRegistration { registration }",
     "coordinator::tests::strongly_capturing_sink_release_drops_the_coordinator_and_stops_the_timer"),
    # gate7 P1 #1: the exit flag is the observable half of the timer-thread
    # stop; a worker that returns without flipping it hides the leak.
    ("timer-exit-flag-never-set", "replay.rs",
     "                if worker.shutdown {\n                    exited.store(true, Ordering::SeqCst);\n                    return;\n                }",
     "                if worker.shutdown {\n                    let _ = &exited;\n                    return;\n                }",
     "coordinator::tests::strongly_capturing_sink_release_drops_the_coordinator_and_stops_the_timer"),
    # gate7 P1 #1: a dropped registration must reject later emits, not accept
    # them into a replay with no live sink.
    ("registration-drop-accepted", "coordinator.rs",
     "                state.sinks.remove(session_key);\n                return false;\n            };",
     "                return true;\n            };",
     "coordinator::tests::dropping_the_registration_unregisters_the_sink_but_keeps_the_coordinator"),
    # --- fix round 8: codex-r8 P1 #1 (registration liveness vs sink liveness)
    # and P1 #2 (documented ownership contract + explicit unregister); each
    # new pin must be falsifiable. ---
    # P1 #1 interleaving ①: a drainer parked inside the sink holds the strong
    # sink handle; reinstating the pre-r8 sink-liveness check (via the drain
    # state of the in-flight delivery) accepts the post-drop event.
    ("emit-accepts-while-in-flight", "coordinator.rs",
     EMIT_LOOKUP,
     EMIT_ACCEPTS_IN_FLIGHT,
     "coordinator::tests::registration_check_fails_while_a_delivery_is_in_flight"),
    # P1 #1 single-threaded variant ②: the sink drops its own registration
    # and re-enters emit; reinstating the sink-liveness check (via the still
    # present queue entry) accepts the reentrant event.
    ("emit-accepts-while-queued", "coordinator.rs",
     EMIT_LOOKUP,
     EMIT_ACCEPTS_QUEUED,
     "coordinator::tests::sink_dropping_its_own_registration_rejects_the_reentrant_emit"),
    # P1 #2: unregister must actually remove the map entry, or a live
    # registration keeps delivering after the explicit detach.
    ("unregister-keeps-sink", "coordinator.rs",
     "    pub fn unregister(&self, session_key: &JsString) {\n        lock(&self.state).sinks.remove(session_key);\n    }",
     "    pub fn unregister(&self, session_key: &JsString) {\n        let _ = session_key;\n    }",
     "coordinator::tests::unregister_detaches_the_sink_without_touching_the_replay"),
    # P1 #2 conforming host: any strong coordinator->sink edge that outlives
    # the registration blocks the plain-drop release the contract promises.
    # The edge must live past the coordinator's own Drop (a parked delivery
    # entry dies with the coordinator, which is not a leak here), so the
    # mutant parks the sink in a process-wide static instead.
    ("weak-host-release-blocked-by-strong-sink", "coordinator.rs",
     START_INSERT_AND_RETURN,
     "        static MUTATION_LEAK: Mutex<Vec<Sink>> = Mutex::new(Vec::new());\n"
     "        MUTATION_LEAK\n"
     "            .lock()\n"
     "            .unwrap()\n"
     "            .push(Arc::clone(&registration.sink));\n"
     "        state\n"
     "            .sinks\n"
     "            .insert(session_key.clone(), Arc::downgrade(&registration));\n"
     "        TurnSinkRegistration { registration }",
     "coordinator::tests::weakly_capturing_host_releases_the_sink_coordinator_and_timer_on_drop"),
    # P1 #2 counter-example host: the same strong edge also blocks the release
    # after the explicit teardown (unregister + letting go of the strong
    # owner) — the sink never drops.
    ("strong-host-release-blocked-by-strong-sink", "coordinator.rs",
     START_INSERT_AND_RETURN,
     "        state\n"
     "            .sinks\n"
     "            .insert(session_key.clone(), Arc::downgrade(&registration));\n"
     "        state\n"
     "            .deliveries\n"
     "            .entry(JsString::from(\"mutation-leak\"))\n"
     "            .or_default()\n"
     "            .push_back((Arc::clone(&registration.sink), JsValue::Null, 0));\n"
     "        TurnSinkRegistration { registration }",
     "coordinator::tests::strongly_capturing_host_leaks_on_drop_and_releases_through_explicit_unregister"),
    # --- fix round 9 (#805): a panicking injected ReplayTimer must unwind
    # outside every state guard, and a state lock poisoned elsewhere must not
    # wedge the store or the drain cleanup. Each carried-guard mutant puts the
    # panic back inside the guard (poisoning the shared lock); the two
    # lock-removal mutants put the `.unwrap()` panic back; the swallowed and
    # skip-poison mutants break the panic surfacing / poisoned cleanup. ---
    ("timer-panic-carried-by-start-guard", "replay.rs",
     START_CANCEL_GUARDED, START_CANCEL_PLAIN,
     "replay::tests::timer_panic_in_start_keeps_the_store_usable"),
    ("timer-panic-carried-by-retain-guard", "replay.rs",
     RETAIN_SET_GUARDED, RETAIN_SET_PLAIN,
     "replay::tests::timer_panic_in_retain_leaves_a_terminal_replay_without_a_timer"),
    # The same retain guard, swallowing instead of carrying: the caller no
    # longer sees the host timer panic at the call site like TS would.
    ("timer-panic-swallowed-by-retain", "replay.rs",
     RETAIN_SET_GUARDED, RETAIN_SET_SWALLOWED,
     "replay::tests::timer_panic_in_retain_leaves_a_terminal_replay_without_a_timer"),
    ("timer-panic-carried-by-clear-guard", "replay.rs",
     CLEAR_CANCEL_GUARDED, CLEAR_CANCEL_PLAIN,
     "replay::tests::timer_panic_in_clear_terminal_keeps_the_store_usable"),
    ("timer-panic-carried-by-dispose-guard", "replay.rs",
     DISPOSE_CANCEL_ALL_GUARDED, DISPOSE_CANCEL_ALL_PLAIN,
     "replay::tests::timer_panic_in_dispose_keeps_the_store_usable"),
    # P1: every state entry must recover the guard from a poisoned lock.
    ("poison-tolerant-state-lock-removed", "cleanup_task.rs",
     LOCK_HELPER, LOCK_HELPER_UNWRAP,
     "replay::tests::poisoned_replay_state_lock_still_serves_every_session"),
    ("coordinator-timer-panic-carried-by-start-guard", "coordinator.rs",
     COORDINATOR_START_GUARDED, COORDINATOR_START_PLAIN,
     "coordinator::tests::timer_panic_in_coordinator_start_keeps_state_usable"),
    ("coordinator-timer-panic-carried-by-retain-guard", "coordinator.rs",
     COORDINATOR_RETAIN_GUARDED, COORDINATOR_RETAIN_PLAIN,
     "coordinator::tests::timer_panic_in_coordinator_retain_keeps_state_usable"),
    ("coordinator-timer-panic-carried-by-dispose-guard", "coordinator.rs",
     COORDINATOR_DISPOSE_GUARDED, COORDINATOR_DISPOSE_PLAIN,
     "coordinator::tests::timer_panic_in_coordinator_dispose_keeps_state_usable"),
    ("coordinator-poison-tolerant-lock-removed", "cleanup_task.rs",
     LOCK_HELPER, LOCK_HELPER_UNWRAP,
     "coordinator::tests::poisoned_coordinator_state_lock_still_serves_every_session"),
    # The drain cleanup guard must use the same tolerant lock: skipping the
    # cleanup on a poisoned state strands the `draining` entry and the second
    # emit never delivers.
    ("drain-guard-skips-poisoned-cleanup", "coordinator.rs",
     DRAIN_GUARD_TOLERANT, DRAIN_GUARD_SKIPS_POISON,
     "coordinator::tests::poisoned_coordinator_state_lock_still_serves_every_session"),
]
for name, filename, before, after, _test in mutants:
    assert originals[filename].count(before) == 1, (name, originals[filename].count(before))

if requested - {m[0] for m in mutants}:
    sys.exit(f"unknown mutants: {sorted(requested - {m[0] for m in mutants})}")
selected = [m for m in mutants if not requested or m[0] in requested]
paths = {"cleanup_task.rs": cleanup, "replay.rs": turn / "replay.rs",
         "coordinator.rs": turn / "coordinator.rs", "oracle.json": oracle_path}


def run(name, test):
    slots = wait_for_slot(rust.parent / "scripts/cargo-slots.sh")
    with (output / f"{name}.slots.log").open("x") as log:
        log.write(slots.stdout + slots.stderr)
    result = run_owned(["cargo", "test", "--offline", "--manifest-path", str(rust / "Cargo.toml"),
                        "-p", "pilotdeck-gateway", "--lib", "runtime::turn::" + test,
                        "--", "--exact"], cwd=rust.parent, env=env, timeout=600)
    raw = result.stdout + result.stderr
    with (output / f"{name}.log").open("x") as log:
        log.write(raw)
    return result, raw


summary = []
for name, filename, before, after, test in selected:
    path = paths[filename]
    with mutating_sources(rust.parent, [path]) as snapshots:
        original = snapshots[path].decode()
        assert original.count(before) == 1, (name, "stale anchor")
        result, raw = run(name + "-positive", test)
        assert result.returncode == 0 and "1 passed" in raw, (name, "positive failed")
        path.write_text(original.replace(before, after, 1))
        result, raw = run(name, test)
        assert (result.returncode == 101 and "test result: FAILED" in raw
                and f"{test} ... FAILED" in raw and "could not compile" not in raw), (
                    name, "not a targeted runtime failure")
    with mutating_sources(rust.parent, [path]):
        result, raw = run(name + "-restored", test)
        assert result.returncode == 0 and "1 passed" in raw, (name, "restore failed")
    summary.append({"name": name, "test": test, "killed": True, "restored": True})
    print(f"{name}: KILLED; restored PASS", flush=True)
with (output / "summary.json").open("x") as log:
    json.dump(summary, log, indent=2)
print(f"{len(summary)} mutants killed and restored", flush=True)

#!/usr/bin/env python3
"""Temporary product mutations; require an oracle assertion failure, then restore.

#1442/#1599/#1600 negative controls: each mutation puts one host-supplied
callback (the resolver, the settle hook, the accepts validator, or the change
observer) back under the GatewayUserDialogBus MutexGuard, drifts the shared
`management_tests::setup` fixture back to the retired unconnected bus, or
drops the identity binding that keeps an answer bound to the dialog it was
validated against. The matching probe test must turn red. Every mutant keeps
the callback call count and order of the original code, so the kill evidence
is the "runs under the guard" probe rather than a duplicated callback. Run
with no other writers/builds in this worktree. Evidence goes in a new output
folder; original source bytes and hashes are verified after every mutation.

Phases per mutation (specs/negative-controls/README.md): a green baseline for
the bound test (positive) -> the mutant must die at its own assertion
(negative) -> the restored bytes must run green again (restored). Any phase
failing records INVALID, every other mutation still runs.

The declared denominator [`DECLARED`] is a literal in this file, never the
runtime table: emptying or trimming `mutations` is drift, and drift publishes
a suite-level INVALID row and exits non-zero (the empty-table clause of the
negative-control contract, #996 fix-r6). An explicit argv subset is a user
request and stays green when every selected row is KILLED.

Remote-host mode keeps the delivery uncommitted, so the baseline is supplied
by the caller (argv[2]): a `git stash create` commit of the staged delivery
tree, never a pushed ref.
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_failed, panic_evidence
from negative_selection import baseline_passed
from negative_process import mutating_sources, run_owned, wait_for_slot as gate_wait_for_slot

root = Path(__file__).resolve().parents[5]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
baseline = sys.argv[2]
source = root / 'src/pilotdeck-gateway/src'
LOCK_ASSERTION = 'must not run under the guard'
FIXTURE_ASSERTION = "the shared fixture's user-dialog bus must be connected"
SETTLE_TEST = ('native_process::methods::tests::'
               'user_dialog_settle_and_resolve_reenter_the_bus_lock_free')
OBSERVER_TEST = ('native_process::methods::tests::'
                 'user_dialog_observer_reenters_the_bus_lock_free')
ACCEPTS_TEST = ('native_process::methods::tests::'
                'user_dialog_accepts_reenters_the_bus_lock_free')
FIXTURE_TEST = ('native_process::management_tests::'
                'setup_assembles_a_connected_user_dialog_bus')
CHANNEL_TEST = ('user_dialog::channel_tests::'
                'user_dialog_channel_cancel_reenters_the_bus_lock_free')
CROSS_TEST = ('user_dialog::channel_tests::'
              'cross_gateway_answer_is_not_delivered_to_a_dialog_replaced_during_the_take')
CROSS_ASSERTION = ('#1599 a cross-gateway answer must not be delivered to a '
                   'dialog replaced during take_stored_answer')
REPLACED_TEST = ('user_dialog::tests::'
                 'consume_for_response_refuses_a_dialog_replaced_after_the_snapshot')
REPLACED_ASSERTION = ('#1599 an answer validated against a replaced dialog '
                      'must not be delivered')
RESTORE_TEST = ('user_dialog::tests::'
                'restore_keeps_the_consumed_dialog_at_its_original_position')
RESTORE_ASSERTION = ('#1599 restore puts the consumed dialog back at its '
                     'original position')
RESTORE_CONFLICT_TEST = ('user_dialog::tests::'
                         'restore_keeps_a_registration_that_replaced_the_consumed_dialog')
RESTORE_CONFLICT_ASSERTION = ('#1599 restore must not overwrite the reentrant '
                              'registration')
RESTORE_DROPPED_ASSERTION = ('#1599 the displaced waiter comes back to be ended')
LATE_TEST = ('native_process::methods::tests::'
             'user_dialog_respond_without_a_snapshot_leaves_a_late_registration_pending')
LATE_ASSERTION = ('#1599 an answered respond with no peeked dialog must report '
                  'not delivered')
REPLACED_METHOD_TEST = ('native_process::methods::tests::'
                        'user_dialog_respond_reports_not_delivered_when_accepts_replaces_'
                        'the_dialog')
REPLACED_METHOD_ASSERTION = ('#1599 an answer validated against a replaced '
                             'dialog must not be delivered')
RESTORE_POSITION_TEST = ('user_dialog::tests::'
                        'restore_keeps_the_relative_position_after_a_reentrant_removal')
RESTORE_POSITION_ASSERTION = ('#1599 restore keeps the consumed dialog before '
                              'the member that followed it')
SETTLE_EVENTS_TEST = ('native_process::methods::tests::'
                      'user_dialog_respond_settle_failure_still_delivers_the_expired_lease')
SETTLE_EVENTS_ASSERTION = ('#1599 a failing settle hook still delivers the lease '
                           'expiry it did not roll back')
REPLACED_POLL_TEST = ('user_dialog::channel_tests::'
                     'a_replaced_dialog_keeps_its_own_cross_gateway_poll')
REPLACED_POLL_ASSERTION = '#1599 a replaced dialog keeps its own cross-gateway poll'
STORE_WRITE_TEST = ('user_dialog::channel_tests::'
                    'a_registration_replaced_during_its_store_write_ends_without_'
                    'publishing_a_bus_entry')
STORE_WRITE_ASSERTION = ('#1599 a registration replaced during its store write '
                         'must end with the replacement error')
CLAIM_PUBLISH_TEST = ('user_dialog::channel_tests::'
                      'a_registration_replaced_during_its_owner_claim_ends_without_'
                      'publishing_a_bus_entry')
# #2027: `begin` now re-reads the sticky replaced flag under the bus guard
# before publishing, so dropping the post-`store.put` re-check no longer lets
# the original publish over the replacement. The waiter still ends, one stage
# later and with the takeover error, and the bound test dies there.
CLAIM_PUBLISH_ASSERTION = ('#1599 a registration replaced during its owner claim '
                           'must end with the replacement error')
OBSERVER_REPLACE_TEST = ('user_dialog::channel_tests::'
                         'a_registration_replaced_by_the_change_observer_ends_'
                         'without_installing_a_poll')
OBSERVER_REPLACE_ASSERTION = ('#1599 a registration replaced by the change '
                              'observer must end with the replacement error')
ABORT_REPLACE_TEST = ('user_dialog::channel_tests::'
                      'an_abort_from_a_superseded_registration_leaves_the_'
                      'replacement_pending')
ABORT_REPLACE_ASSERTION = ('#1599 an abort from a superseded registration must '
                           'not cancel the replacement')
SETTLE_RELEASE_TEST = ('user_dialog::channel_tests::'
                       'a_settle_that_releases_into_a_replacement_keeps_the_'
                       'replacement_rows')
SETTLE_RELEASE_ASSERTION = ("#1599 a superseded settle must not delete the "
                            "replacement's store row")
SETTLE_JOURNAL_TEST = ('user_dialog::channel_tests::'
                       'a_settle_that_removes_its_journal_into_a_replacement_'
                       'keeps_the_replacement_rows')
SETTLE_JOURNAL_ASSERTION = ("#1599 a superseded settle must not delete the "
                            "replacement's store row")
JOURNAL_WRITE_TEST = ('user_dialog::channel_tests::a_registration_replaced_while_'
                      'its_journal_write_runs_keeps_the_replacement_row')
JOURNAL_WRITE_ASSERTION = ('#1599 a registration replaced during its journal '
                           'write must end with the replacement error')
STORE_CLAIM_ASSERTION = ('#1599 the original must not claim the owner lease '
                         'after the replacement published')
STICKY_REPLACED_TEST = ('user_dialog::channel_tests::a_registration_whose_'
                        'replacement_finished_still_sees_the_supersession')
STICKY_REPLACED_ASSERTION = ('#1599 a registration replaced by a finished '
                             'replacement must end with an error')
CLOCK_HOOK_TEST = ('user_dialog::channel_tests::a_registration_replaced_by_the_'
                   'clock_hook_never_writes_over_the_replacement')
CLOCK_HOOK_ASSERTION = ('#1599 a registration replaced before its persist must '
                        'end with the replacement error')
# Statically declared denominator: a literal tuple, deliberately not derived
# from `mutations`, so a runtime table that lost rows cannot shrink it.
DECLARED = (
    'resolve-back-under-guard',
    'settle-hook-back-under-guard',
    'accepts-back-under-guard',
    'observer-delivery-back-under-guard',
    'fixture-drifts-to-unconnected-bus',
    'channel-cancel-settle-back-under-guard',
    'replaced-dialog-still-consumed',
    'restore-appends-to-the-tail',
    'cross-gateway-replaced-dialog-still-consumed',
    'restore-overwrites-a-reentrant-registration',
    'answered-without-snapshot-consumes-the-late-registration',
    'answered-snapshot-identity-dropped',
    'restore-ignores-the-successor',
    'restore-drops-the-replaced-waiter',
    'settle-failure-drops-pre-consume-events',
    'replaced-dialog-loses-its-poll',
    'superseded-registration-publishes-a-bus-entry',
    'superseded-registration-installs-its-poll',
    'superseded-abort-cancels-the-replacement',
    'settle-release-deletes-the-replacement-row',
    'settle-journal-deletes-the-replacement-row',
    'superseded-registration-overwrites-the-journal-row',
    'superseded-registration-claims-the-replacement-lease',
    'supersession-flag-forgets-a-finished-replacement',
    'replacement-before-persist-overwrites-the-rows',
)
# #2027: `cancel_owned` now decides by two independent checks -- the sticky
# superseded flag at the top and the generation binding inside the bus guard.
# Either check alone leaves a same-id replacement's entry alone, so this
# mutant drops both; with only one dropped the bound test stays green and
# the row would stop proving the invariant.
CANCEL_OWNED_HEAD = """\
    fn cancel_owned(&self, state: &Arc<RequestState>, reason: &str) {
        if self.superseded(state) {
            return;
        }
        if state.settled.load(Ordering::SeqCst) {
            return;
        }
        if !state.registered.load(Ordering::SeqCst) {
            // The request is between the persist gate and `registered = true`.
            // Only raise the flag: removing the state here would make the
            // post-registration cancel a no-op and leak the bus entry, owner
            // lease, journal record and store record. `begin` settles it once
            // the entry is registered.
            state
                .cancelled_before_registration
                .store(true, Ordering::SeqCst);
            return;
        }
        let _in_flight = InFlight::enter(state.clone());
        // #1442: the settle hook, the resolve callback and the change
        // observer are host code; none of them run under the bus guard. TS
        // `remove()` runs onSettled before its mutations, and a throw leaves
        // the dialog pending and broadcast-free (fail-closed,
        // GatewayUserDialogBus.ts:219-226), so a settle failure restores the
        // consumed entry instead of resolving.
        let (consumed, pre, outbox, takeover) = {
            let mut bus = self.inner.options.bus.lock().unwrap();
            // `pre` holds changes queued before this consume (normally
            // empty here); a lease that expires inside `consume` lands in
            // `outbox`. A settle failure rolls back only the entry and its
            // own "settled" broadcast, so `outbox` is delivered through
            // `without_settled` (GatewayUserDialogBus.ts:219-226).
            let pre = bus.drain_events();
            // #2027: `bus.consume` is keyed by request id alone. A same-id
            // registration published after this channel's ownership checks
            // (another Gateway on this bus, or a replacement on another
            // thread) owns the entry now; consuming it would delete that
            // dialog and leave its waiter with nothing to answer through.
            // Both calls share the guard, so no publication can land between
            // them (window 3 sequence A).
            let own = bus.pending_matches(
                &self.inner.options.session_key,
                &state.request_id,
                state.generation.load(Ordering::SeqCst),
            );
"""
CANCEL_OWNED_HEAD_UNBOUND = """\
    fn cancel_owned(&self, state: &Arc<RequestState>, reason: &str) {
        if false && self.superseded(state) {
            return;
        }
        if state.settled.load(Ordering::SeqCst) {
            return;
        }
        if !state.registered.load(Ordering::SeqCst) {
            // The request is between the persist gate and `registered = true`.
            // Only raise the flag: removing the state here would make the
            // post-registration cancel a no-op and leak the bus entry, owner
            // lease, journal record and store record. `begin` settles it once
            // the entry is registered.
            state
                .cancelled_before_registration
                .store(true, Ordering::SeqCst);
            return;
        }
        let _in_flight = InFlight::enter(state.clone());
        // #1442: the settle hook, the resolve callback and the change
        // observer are host code; none of them run under the bus guard. TS
        // `remove()` runs onSettled before its mutations, and a throw leaves
        // the dialog pending and broadcast-free (fail-closed,
        // GatewayUserDialogBus.ts:219-226), so a settle failure restores the
        // consumed entry instead of resolving.
        let (consumed, pre, outbox, takeover) = {
            let mut bus = self.inner.options.bus.lock().unwrap();
            // `pre` holds changes queued before this consume (normally
            // empty here); a lease that expires inside `consume` lands in
            // `outbox`. A settle failure rolls back only the entry and its
            // own "settled" broadcast, so `outbox` is delivered through
            // `without_settled` (GatewayUserDialogBus.ts:219-226).
            let pre = bus.drain_events();
            // #2027: `bus.consume` is keyed by request id alone. A same-id
            // registration published after this channel's ownership checks
            // (another Gateway on this bus, or a replacement on another
            // thread) owns the entry now; consuming it would delete that
            // dialog and leave its waiter with nothing to answer through.
            // Both calls share the guard, so no publication can land between
            // them (window 3 sequence A).
            let own = true;
"""
# Each independent mutant is applied to the original, never cumulatively.
mutations = [
    # respond: the resolver runs while a fresh guard is alive again.
    ('resolve-back-under-guard', 'native_process/methods/user_dialog.rs',
     '                            outbox.deliver();\n'
     '                            (consumed.entry.resolve)(answer);',
     '                            outbox.deliver();\n'
     '                            let _guard = self\n'
     '                                .user_dialog_bus\n'
     '                                .lock()\n'
     '                                .unwrap_or_else(|e| e.into_inner());\n'
     '                            (consumed.entry.resolve)(answer);',
     SETTLE_TEST, LOCK_ASSERTION),
    # consume: the settle hook runs with a guard alive again (called once).
    ('settle-hook-back-under-guard', 'native_process/methods/user_dialog.rs',
     '                            let settled = consumed\n'
     '                                .entry\n'
     '                                .on_settled\n'
     '                                .as_ref()\n'
     '                                .map_or(Ok(()), |hook| hook());',
     '                            let settled = {\n'
     '                                let _guard = self\n'
     '                                    .user_dialog_bus\n'
     '                                    .lock()\n'
     '                                    .unwrap_or_else(|e| e.into_inner());\n'
     '                                consumed\n'
     '                                    .entry\n'
     '                                    .on_settled\n'
     '                                    .as_ref()\n'
     '                                    .map_or(Ok(()), |hook| hook())\n'
     '                            };',
     SETTLE_TEST, LOCK_ASSERTION),
    # respond: the accepts validator runs with a guard alive again (called once).
    ('accepts-back-under-guard', 'native_process/methods/user_dialog.rs',
     '                        if !(snapshot.accepts)(&value) {',
     '                        let accepted = {\n'
     '                            let _guard = self\n'
     '                                .user_dialog_bus\n'
     '                                .lock()\n'
     '                                .unwrap_or_else(|e| e.into_inner());\n'
     '                            (snapshot.accepts)(&value)\n'
     '                        };\n'
     '                        if !accepted {',
     ACCEPTS_TEST, LOCK_ASSERTION),
    # emit: the observer is invoked in place instead of being queued.
    ('observer-delivery-back-under-guard', 'user_dialog/bus.rs',
     '    fn emit(&mut self, change: JsValue) {\n'
     '        self.outbox.push(change);\n'
     '    }',
     '    fn emit(&mut self, change: JsValue) {\n'
     '        let observer = self.on_change.clone();\n'
     '        if let Some(observer) = observer {\n'
     '            let _ = observer(change);\n'
     '        }\n'
     '    }',
     OBSERVER_TEST, LOCK_ASSERTION),
    # #1600: the shared fixture drifts back to the retired unconnected bus.
    # fix-r12 前置修复：#2692 fix-r3 之后 management_tests.rs 有了第二处
    # 同形 user_dialog_bus 块（:350），裸块锚点计数变 2 让自查整红；锚点
    # 前缀并入仅此一处存在的 #1600 注释行恢复唯一性（old/after 同带注释，
    # 变异只改 bus 构造本身）。
    ('fixture-drifts-to-unconnected-bus', 'native_process/management_tests.rs',
     '            // #1600: same state production assembles\n'
     '            // (`native_process/assembly/user_dialog.rs`): connected, no\n'
     '            // renderer observer. The legacy observer-coupled `new(runtime,\n'
     '            // None)` made the shared fixture diverge from production, so any\n'
     '            // case relying on "the bus is unavailable" here would test a state\n'
     '            // that no longer exists. A case that genuinely needs the\n'
     '            // unconnected state builds it explicitly.\n'
     '            user_dialog_bus: Arc::new(std::sync::Mutex::new(\n'
     '                crate::user_dialog::GatewayUserDialogBus::connected(\n'
     '                    crate::user_dialog::DialogRuntime::default(),\n'
     '                ),\n'
     '            )),',
     '            // #1600: same state production assembles\n'
     '            // (`native_process/assembly/user_dialog.rs`): connected, no\n'
     '            // renderer observer. The legacy observer-coupled `new(runtime,\n'
     '            // None)` made the shared fixture diverge from production, so any\n'
     '            // case relying on "the bus is unavailable" here would test a state\n'
     '            // that no longer exists. A case that genuinely needs the\n'
     '            // unconnected state builds it explicitly.\n'
     '            user_dialog_bus: Arc::new(std::sync::Mutex::new(\n'
     '                crate::user_dialog::GatewayUserDialogBus::new(\n'
     '                    crate::user_dialog::DialogRuntime::default(),\n'
     '                    None,\n'
     '                ),\n'
     '            )),',
     FIXTURE_TEST, FIXTURE_ASSERTION),
    # channel.cancel: the settle hook runs with a guard alive again.
    ('channel-cancel-settle-back-under-guard', 'user_dialog/channel.rs',
     '            Some(consumed) => {\n'
     '                let settled = consumed\n'
     '                    .entry\n'
     '                    .on_settled\n'
     '                    .as_ref()\n'
     '                    .map_or(Ok(()), |hook| hook());',
     '            Some(consumed) => {\n'
     '                let settled = {\n'
     '                    let _guard = self.inner.options.bus.lock().unwrap();\n'
     '                    consumed\n'
     '                        .entry\n'
     '                        .on_settled\n'
     '                        .as_ref()\n'
     '                        .map_or(Ok(()), |hook| hook())\n'
     '                };',
     CHANNEL_TEST, LOCK_ASSERTION),
    # consume_for_response: the snapshot identity check is dropped, so an
    # answer validated against a replaced dialog reaches the new one.
    ('replaced-dialog-still-consumed', 'user_dialog/bus.rs',
     '        if validated\n'
     '            .is_some_and(|snapshot| !self.pending_matches(session, request, snapshot.generation))\n'
     '        {',
     '        if validated.is_some_and(|_snapshot| false) {',
     REPLACED_TEST, REPLACED_ASSERTION),
    # restore: the entry goes back to the bucket tail instead of its position.
    ('restore-appends-to-the-tail', 'user_dialog/bus.rs',
     '        bucket.insert(position, entry);',
     '        bucket.push(entry);',
     RESTORE_TEST, RESTORE_ASSERTION),
    # channel cross-gateway poll: the identity check is dropped, so an answer
    # taken for one dialog is consumed from whatever is pending under the id.
    ('cross-gateway-replaced-dialog-still-consumed', 'user_dialog/channel.rs',
     '            let replaced =\n'
     '                bus.pending_replaced(&self.inner.options.session_key, request_id, generation);',
     '            let _ = bus.pending_replaced(\n'
     '                &self.inner.options.session_key,\n'
     '                request_id,\n'
     '                generation,\n'
     '            );\n'
     '            let replaced = false;',
     CROSS_TEST, CROSS_ASSERTION),
    # restore: the conflict check is dropped, so a settle hook that reentered
    # and registered the same request id is overwritten and its lease lost.
    ('restore-overwrites-a-reentrant-registration', 'user_dialog/bus.rs',
     '        if bucket\n'
     '            .iter()\n'
     '            .any(|value| value.request_id == entry.request_id)\n'
     '        {\n'
     '            return Some(entry);\n'
     '        }\n'
     '        let request_id = entry.request_id.clone();',
     '        let request_id = entry.request_id.clone();',
     RESTORE_CONFLICT_TEST, RESTORE_CONFLICT_ASSERTION),
    # respond: the empty-snapshot guard is dropped, so an answered response
    # consumes whatever was registered under the id after the peek.
    ('answered-without-snapshot-consumes-the-late-registration',
     'native_process/methods/user_dialog.rs',
     '                    if answered && snapshot.is_none() {',
     '                    if answered && snapshot.is_none() && false {',
     LATE_TEST, LATE_ASSERTION),
    # respond: the answered path stops passing the validated snapshot, so the
    # consume accepts whatever registration now owns the request id.
    ('answered-snapshot-identity-dropped', 'native_process/methods/user_dialog.rs',
     '                            if answered { snapshot.as_ref() } else { None },',
     '                            None,',
     REPLACED_METHOD_TEST, REPLACED_METHOD_ASSERTION),
    # restore: the old successor is ignored, so an entry whose predecessor was
    # consumed by a reentrant callback goes back to the tail instead of the
    # slot it occupied (Grok boundary P1-2).
    ('restore-ignores-the-successor', 'user_dialog/bus.rs',
     '        let position = successor\n'
     '            .and_then(|after| bucket.iter().position(|value| value.request_id == after))',
     '        let position = None::<JsString>\n'
     '            .and_then(|after| bucket.iter().position(|value| value.request_id == after))',
     RESTORE_POSITION_TEST, RESTORE_POSITION_ASSERTION),
    # restore: the displaced waiter is dropped silently again, so the tool call
    # that owned it never ends.
    ('restore-drops-the-replaced-waiter', 'user_dialog/bus.rs',
     '            return Some(entry);\n'
     '        }\n'
     '        let request_id = entry.request_id.clone();',
     '            return None;\n'
     '        }\n'
     '        let request_id = entry.request_id.clone();',
     RESTORE_CONFLICT_TEST, RESTORE_DROPPED_ASSERTION),
    # respond: a failing settle hook drops every queued change, including the
    # lease expiry that already happened inside consume_for_response
    # (Grok boundary P1-1).
    ('settle-failure-drops-pre-consume-events',
     'native_process/methods/user_dialog.rs',
     '                                pre.deliver();\n'
     '                                outbox.without_settled(&session_key, &request_id).deliver();',
     '',
     SETTLE_EVENTS_TEST, SETTLE_EVENTS_ASSERTION),
    # channel.poll: the old request's stop deletes a same-id replacement's
    # poll entry again (TS keeps one timer per request).
    ('replaced-dialog-loses-its-poll', 'user_dialog/channel.rs',
     '.is_some_and(|entry| entry.registration == registration)',
     '.is_some_and(|entry| entry.registration >= registration)',
     REPLACED_POLL_TEST, REPLACED_POLL_ASSERTION),
    # channel.begin: a registration replaced by a reentrant owner claim still
    # publishes its bus entry, taking the bucket back from the replacement.
    # #1599 r11（GLM boundary P1 + DeepSeek sibling/boundary P1）：这一行只能被
    # `claim_stored_owner` 内的重入触发——`store.put` 内的重入会先被 persist 自己
    # 的 store-write 复查拦下（`channel.rs` persist），begin 根本走不到这里；原先
    # 绑在 store-write 用例上时变异体逐字节等价，整行 SURVIVED。
    ('superseded-registration-publishes-a-bus-entry', 'user_dialog/channel.rs',
     'if self.end_if_superseded(&state, "store write") {',
     'if false && self.end_if_superseded(&state, "store write") {',
     CLAIM_PUBLISH_TEST, CLAIM_PUBLISH_ASSERTION),
    # channel.begin: a registration replaced by the change observer installs
    # its poll anyway, so its waiter never ends and no bus entry can answer it.
    ('superseded-registration-installs-its-poll', 'user_dialog/channel.rs',
     'if self.end_if_superseded(&state, "timer read") {',
     'if false && self.end_if_superseded(&state, "timer read") {',
     OBSERVER_REPLACE_TEST, OBSERVER_REPLACE_ASSERTION),
    # channel.cancel_owned: the abort callback cancels by request id again, so
    # one superseded registration consumes the replacement's bus entry.
    ('superseded-abort-cancels-the-replacement', 'user_dialog/channel.rs',
     CANCEL_OWNED_HEAD, CANCEL_OWNED_HEAD_UNBOUND,
     ABORT_REPLACE_TEST, ABORT_REPLACE_ASSERTION),
    # settle: the re-check after `release` is dropped, so the superseded
    # request deletes the rows the replacement wrote under the same id.
    ('settle-release-deletes-the-replacement-row', 'user_dialog/channel.rs',
     '        // `release` is host code; a reentrant callback may have registered the\n'
     '        // same id and written its own journal/store rows. Stop before deleting\n'
     '        // the replacement\'s records (#1599).\n'
     '        if superseded_by_registration(inner, state) {\n'
     '            return Ok(());\n'
     '        }',
     '',
     SETTLE_RELEASE_TEST, SETTLE_RELEASE_ASSERTION),
    # settle: the same re-check after `journal.remove`.
    ('settle-journal-deletes-the-replacement-row', 'user_dialog/channel.rs',
     '        journal.remove(&state.request_id)?;\n'
     '        // Same window as above, one destructive step later.\n'
     '        if superseded_by_registration(inner, state) {\n'
     '            return Ok(());\n'
     '        }',
     '        journal.remove(&state.request_id)?;',
     SETTLE_JOURNAL_TEST, SETTLE_JOURNAL_ASSERTION),
    # persist: the re-check after `journal.record` is dropped, so a superseded
    # registration writes its own store row over the replacement's.
    ('superseded-registration-overwrites-the-journal-row', 'user_dialog/channel.rs',
     '            if replaced(&self.inner, state) {\n'
     '                return Err(DialogError::Invalid(replaced_during("journal write")));\n'
     '            }\n',
     '',
     JOURNAL_WRITE_TEST, JOURNAL_WRITE_ASSERTION),
    # persist: the re-check after `store.put` is dropped, so a superseded
    # registration runs the host claim hook and steals the replacement's lease.
    ('superseded-registration-claims-the-replacement-lease', 'user_dialog/channel.rs',
     '            if replaced(&self.inner, state) {\n'
     '                return Err(DialogError::Invalid(replaced_during("store write")));\n'
     '            }\n',
     '',
     STORE_WRITE_TEST, STORE_CLAIM_ASSERTION),
    # begin: the sticky flag is never set, so a suspended original mistakes the
    # finished replacement's empty table slot for "the id is still mine".
    ('supersession-flag-forgets-a-finished-replacement', 'user_dialog/channel.rs',
     '                previous.replaced.store(true, Ordering::SeqCst);\n',
     '',
     STICKY_REPLACED_TEST, STICKY_REPLACED_ASSERTION),
    # persist: the pre-write check is dropped, so a replacement registered by a
    # host callback before `persist` (here the `now_ms` clock hook) has its rows
    # overwritten by the original before the later checks notice.
    ('replacement-before-persist-overwrites-the-rows', 'user_dialog/channel.rs',
     '        if replaced(&self.inner, state) {\n'
     '            return Err(DialogError::Invalid(replaced_during("registration")));\n'
     '        }\n',
     '',
     CLOCK_HOOK_TEST, CLOCK_HOOK_ASSERTION),
]
declared = list(DECLARED)
selected = sys.argv[3:]
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR'):
    sys.exit('CARGO_TARGET_DIR must be supplied by the caller')
summary = []
failures = []


def wait_for_slot(name):
    # Single source of truth for the gate contract (retries inside one budget,
    # no bare subprocess timeout); its raw output is retained per mutation.
    slot = gate_wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=1800, interval=5)
    (out / f'{name}.slots.log').write_text(slot.stdout + slot.stderr)


def say(message):
    # A closed pipe must not turn one result row into two (the print sits inside
    # the per-mutation try, so an unguarded BrokenPipeError lands in its except).
    try:
        print(message, flush=True)
    except OSError:
        pass


def publish_summary():
    # Publishes once, after the source guard has restored and verified the bytes
    # (specs/negative-controls/README.md). `total` is the statically declared
    # denominator, never the argv selection, so a subset run and a full run stay
    # distinguishable in the evidence. Rows live under `controls` like the
    # shared `negative_suite.publish_summary`.
    counts = {state: sum(row['status'] == state for row in summary)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    payload = dict(total=len(declared), declared=declared,
                   selected=[row['name'] for row in summary],
                   counts=counts, controls=summary)
    tmp = out / 'summary.json.tmp'
    tmp.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + '\n')
    os.replace(tmp, out / 'summary.json')
    return counts


def suite_row(reason):
    # Denominator drift has no per-mutation row to mark, so it gets its own
    # suite-level INVALID row; otherwise an empty run would publish green.
    summary.append(dict(name='switch-denominator', test=None, assertion=None,
                        status='INVALID', returncode=None, reason=reason,
                        sourceSha256=None))
    failures.append('switch-denominator')


runtime_names = [row[0] for row in mutations]
drift = ([name for name in runtime_names if name not in declared]
         + [name for name in declared if name not in runtime_names])
unknown = [name for name in selected if name not in declared]
if drift or unknown:
    detail = drift + unknown
    suite_row('declared/runtime mutation mismatch: '
              + ', '.join(detail)
              + f'; declared={sorted(declared)} executed={sorted(runtime_names)}')
    publish_summary()
    print(f'switch denominator drifted: {detail}', file=sys.stderr)
    sys.exit(1)
if selected:
    mutations = [row for row in mutations if row[0] in selected]

try:
    with mutating_sources(root, sorted({source / row[1] for row in mutations}),
                          baseline=baseline) as snapshots:

        def run(full):
            result = run_owned(['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
                                '-p', 'pilotdeck-gateway', '--lib',
                                full, '--', '--exact'], cwd=root, env=env, timeout=600)
            return result.returncode, result.stdout + result.stderr

        # #819: an unmutated baseline for every bound test must be green first.
        baseline_ok = set()
        for test in dict.fromkeys(row[4] for row in mutations):
            wait_for_slot('baseline-' + test)
            code, log = run(test)
            (out / f'baseline-{test}.log').write_text(log)
            ok = code == 0 and baseline_passed(log, test)
            print(f'baseline {test}: {"PASS" if ok else "INVALID"}', flush=True)
            if ok:
                baseline_ok.add(test)
            else:
                failures.append('baseline-' + test)

        for name, filename, old, new, test, assertion in mutations:
            path = source / filename
            original = snapshots[path]
            before = hashlib.sha256(original).hexdigest()
            content = original.decode()
            status, reason, code = 'INVALID', 'not run', None
            try:
                # No bare `assert` here: `python3 -O` strips it, and CI's
                # `test_negative_optimize_guards` rejects unregistered bare
                # asserts in fixture scripts (#1666). The guard is the same.
                count = content.count(old)
                if count != 1:
                    raise RuntimeError(f'{name}: anchor appears {count} times')
                changed = content.replace(old, new, 1)
                (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
                    content.splitlines(True), changed.splitlines(True),
                    fromfile=filename, tofile=filename)))
                wait_for_slot(name)
                path.write_text(changed)
                code, log = run(test)
                (out / f'{name}.log').write_text(log)
                evidence = panic_evidence(log, test)
                # Kill evidence is the mutation's own bound assertion: a
                # mutant only counts as KILLED when the assertion the row
                # binds is the one that fired. Generic suite-wide markers are
                # deliberately not accepted, so a mutant cannot borrow a
                # sibling probe's failure (#1599 r4 review).
                marker_ok = assertion in evidence
                killed = (test in baseline_ok and code == 101
                          and named_test_failed(log, test)
                          and marker_ok
                          and 'could not compile' not in log)
                reason = next((line.strip() for line in log.splitlines()
                               if 'panicked at' in line or 'assertion' in line), '')
                status = 'KILLED' if killed else (
                    'SURVIVED' if code == 0 else 'INVALID')
            except Exception as error:
                status = 'INVALID'
                reason = f'{type(error).__name__}: {error}'
            finally:
                path.write_bytes(original)
                if hashlib.sha256(path.read_bytes()).hexdigest() != before:
                    raise RuntimeError(f'byte restoration failed: {filename}')
            # Restored phase: the bytes put back must run the bound test green
            # again, so a mutant that left a side effect cannot pass as killed.
            try:
                wait_for_slot(name + '-restored')
                code_r, log_r = run(test)
                (out / f'{name}-restored.log').write_text(log_r)
                if not (code_r == 0 and baseline_passed(log_r, test)):
                    status = 'INVALID'
                    reason = (reason + '; restored control did not pass'
                              if reason else 'restored control did not pass')
            except Exception as error:
                status = 'INVALID'
                reason = ((reason + f'; restore test: {type(error).__name__}: {error}')
                          if reason else f'restore test: {type(error).__name__}: {error}')
            summary.append(dict(name=name, test=test, assertion=assertion,
                                status=status, returncode=code, reason=reason,
                                sourceSha256=before))
            say(f'{name}: {status} rc={code} {reason}')
            if status != 'KILLED':
                failures.append(name)
except BaseException as error:
    # Same contract as the sibling fixtures (_shared/negative_control.py,
    # user-dialog-guard-order): a guard failure publishes a complete table with
    # every row INVALID instead of the partial green rows. (SIGTERM / SIGKILL
    # bypass this and leave no summary at all — absence is a fail-closed read.)
    reason = f'source guard: {type(error).__name__}: {error}'
    recorded = {row['name'] for row in summary}
    for row in summary:
        row['status'] = 'INVALID'
        row['reason'] = f'{row["reason"]}; {reason}' if row.get('reason') else reason
    for name, _filename, _old, _new, test, assertion in mutations:
        if name not in recorded:
            summary.append(dict(
                name=name, test=test, assertion=assertion,
                status='INVALID', returncode=None, reason=reason,
                sourceSha256=None))
    publish_summary()
    raise

# Runtime truncation check: the loop must have produced exactly one row per
# selected mutation. An emptied table reaches here with no rows at all.
recorded = [row['name'] for row in summary]
missing = [row[0] for row in mutations if row[0] not in recorded]
if not summary or missing or len(summary) != len(mutations):
    suite_row('runtime execution table incomplete: '
              f'missing={missing} rows={len(summary)} expected={len(mutations)}'
              + ('' if summary else '; empty execution table'))
counts = publish_summary()
print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
if failures or any(row['status'] != 'KILLED' for row in summary):
    print(f'not killed: {", ".join(failures) or "see summary"}', file=sys.stderr)
    sys.exit(1)
print(f'{counts["KILLED"]}/{len(declared)} declared mutations killed; '
      'every source restored byte-for-byte', flush=True)

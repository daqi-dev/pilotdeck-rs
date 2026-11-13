#!/usr/bin/env python3
"""#1339 compiled controls: compile every switch once, then run the selected
positive, negative and restored cases with negative_suite verdicts. No source
rewriting, no alternate HEAD and no second target directory. Every switch
mutates exactly one sms-channel behavior and is killed by the oracle replay
cell or dedicated test that pins that behavior's TS bytes (26 → 78; `#1984`
added `sms-oracle-boundary-unregistered`, `#1957` added
`sms-oracle-signed-signature-header`, `#1913` added
`sms-oracle-recorded-version`, `#2152` added
`sms-oracle-recorded-omit-host`, `#2691` added the three port-guard
switches, `#2041` added `sms-oracle-wire-body-len-fork` — the `switches`
table is the truth source). `#1879` only
rewrites existing reads (`recorded_present`
covers `env` / `sendFailAt` / `turn.*` / `truncate` / `halfClose` / `headers` /
`reason` / `extra` now), so it adds no switch. Run one sample in the authoring
session; the coordinator runs the complete list after the patch is committed.

`mutating_sources` cannot be used for a pre-commit change (it requires
src/ == HEAD), so the controls are the #996 compile-time switches the
channel already carries; see the diffs ledger for the deviation note.
"""
import argparse
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch
ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
SMS = ROOT / 'src/pilotdeck-gateway/src/channel/sms'
PREFIX = 'channel::sms::tests::'
# #2133: neg-for-diff.sh finds the tables a diff needs by the literal
# `dir/file` of every changed file (`git grep -F 'sms/mod.rs'`). Rows below
# concatenate a bare file name onto `SMS`, so `sms/<file>` never appears on its
# own and a change under `channel/sms/` would schedule no mutation at all.
# The imported shared helpers (`_shared/mutant_switch.py` and every `fixtures/`
# module its import chain reaches, directly or transitively) and this
# directory's own oracle, generator and README are listed for the same reason:
# the table reads them, and the discovery loop keys on any changed file under
# `fixtures/**`.
# Every bare-filename key this table mutates is declared here; the rows and
# this tuple must agree (fixtures/test_negative_discovery_anchors.py).
DISCOVERY_ANCHORS = (
    'sms/mod.rs', 'sms/gateway.rs', 'sms/mapper.rs', 'sms/render.rs',
    'sms/elicitation.rs', 'sms/permission.rs', 'sms/sender.rs', 'sms/tests.rs',
    'channel-sms/oracle.json', 'channel-sms/generate.mts', 'channel-sms/README.md',
    'fixtures/negative_process.py', 'fixtures/negative_suite.py',
    'fixtures/negative_panic.py', 'fixtures/negative_selection.py',
    'fixtures/pending_reanchor.py', 'fixtures/anchor_selfcheck.py',
    'fixtures/negative_scope.py',
    '_shared/mutant_switch.py',
)
switches = [
    # stop reason replaced by the default: the drain stop of the stop-during-
    # turn cell logs "no reason" instead of the recorded reason.
    ('sms-stop-log-reason', SMS / 'mod.rs', 'stop_during_turn',
     '#1339 sms oracle: stop-during-turn'),
    # TWIML body replaced by the short form: the valid-signature cell's 200
    # response body loses the XML declaration.
    ('sms-twiml-body', SMS / 'mod.rs', 'valid_signature',
     '#1339 sms oracle: valid-signature'),
    # signature gate off: the bad-signature cell is dispatched instead of 403.
    ('sms-signature-gate-off', SMS / 'mod.rs', 'bad_signature',
     '#1339 sms oracle: bad-signature'),
    # empty-From dispatch: the empty-from cell reaches the mapper and turns
    # the TWIML-only record into gateway traffic.
    ('sms-empty-from-dispatch', SMS / 'mod.rs', 'empty_from_and_whitespace_body',
     '#1339 sms oracle: empty-from-and-whitespace-body'),
    # elicitation cancel text replaced: the cancel reply of the
    # elicitation-cancel cell no longer matches TS.
    ('sms-elicit-cancel-text', SMS / 'mod.rs', 'elicitation_cancel',
     '#1339 sms oracle: elicitation-cancel'),
    # active-chat dedupe off: the second message of the active-dedupe cell
    # starts a second turn instead of the status reply.
    ('sms-active-dedupe-off', SMS / 'mod.rs', 'active_dedupe',
     '#1339 sms oracle: active-dedupe'),
    # turn error reply text replaced: the turn-error cell's user-facing
    # failure text no longer matches TS.
    ('sms-error-reply-text', SMS / 'mod.rs', 'turn_error',
     '#1339 sms oracle: turn-error'),
    # in-flight status text replaced: the decide-in-flight-status cell's
    # status reply no longer matches TS.
    ('sms-permission-status-text', SMS / 'permission.rs',
     'permission_decide_in_flight_status',
     '#1339 sms oracle: permission-decide-in-flight-status'),
    # deny confirmation text replaced: the permission-deny cell's confirmation
    # message no longer matches TS.
    ('sms-permission-confirmation-text', SMS / 'permission.rs', 'permission_deny',
     '#1339 sms oracle: permission-deny'),
    # /new command off: the new-session cell keeps the general session key
    # instead of minting the s_ uuid session.
    ('sms-mapper-new-command', SMS / 'mapper.rs', 'new_session',
     '#1339 sms oracle: new-session'),
    # tool-failure fragment dropped: the turn-reply-fragments cell's rendered
    # body loses the "⚠️ ... failed" fragment.
    ('sms-render-failure-fragment', SMS / 'render.rs', 'turn_reply_fragments',
     '#1339 sms oracle: turn-reply-fragments'),
    # fix-r1 P1: signature key order reversed — the recorded TS signatures
    # only replay while the port keeps `Object.keys(params).sort()`.
    ('sms-signature-key-order', SMS / 'mod.rs', 'valid_signature',
     '#1339 sms oracle: valid-signature'),
    # fix-r1 P1: signature URL ignores publicUrl — the public-url cell's
    # recorded signature no longer validates.
    ('sms-signature-public-url', SMS / 'mod.rs', 'start_ok_public_url',
     '#1339 sms oracle: start-ok-public-url'),
    # fix-r1 P2 #6: JSON null falls through to empty params — the dedicated
    # destroy-connection test instead sees a 200 TWIML.
    ('sms-json-null-body', SMS / 'mod.rs', 'json_null_body_is_unhandled_destroy',
     '#1339 json null without signature must destroy the connection'),
    # fix-r1 P2 #8: truncated-body 400 carries the old io::Error text — the
    # body-truncated cell's empty-body 400 no longer matches.
    ('sms-body-error-text', SMS / 'mod.rs', 'body_truncated',
     '#1339 sms oracle: body-truncated'),
    # fix-r1 P2 #9: missing permission requestId becomes the string
    # "undefined" — the recorded decision then carries the key.
    ('sms-permission-request-id-undefined', SMS / 'mod.rs', 'permission_missing_request_id',
     '#1339 sms oracle: permission-missing-request-id'),
    # fix-r1 P2 #9: missing elicitation requestId becomes the string
    # "undefined" — the recorded response then carries the key.
    ('sms-elicit-request-id-empty', SMS / 'elicitation.rs', 'elicitation_missing_request_id',
     '#1339 sms oracle: elicitation-missing-request-id'),
    # fix-r1 P2 #10: the declared-count expectation drifts +1 — the count
    # guard fails on the generator's own declaration.
    ('sms-oracle-declaration-count', SMS / 'tests.rs', 'oracle_case_count_is_recorded',
     '#1339 oracle declared case count'),
    # fix-r1 P2 #11: the logger runs under the boundaries lock — a panicking
    # logger poisons it for the second stop.
    ('sms-log-under-lock', SMS / 'mod.rs', 'logger_panic_does_not_poison_boundaries',
     '#1339 stop after a logger panic must not see a poisoned boundaries mutex'),
    # fix-r1 P2 #13: the replay port preflight is off — the busy-port replay
    # proceeds and its connect probe no longer matches the oracle.
    ('sms-port-preflight', SMS / 'tests.rs', 'port_busy_preflight_fails_fast',
     '#1339 port-busy preflight marker'),
    # fix-r1 P2 #14, fix-r3 P2: the empty webhookHost is not mapped to the
    # dual-stack wildcard — the bind fails instead, so the 127.0.0.1 probe is
    # refused rather than dispatched. The switch returns the failure directly
    # (it used to bind `::1`): a host without IPv6 fell back to the positive
    # run's `0.0.0.0` and the mutant SURVIVED.
    ('sms-empty-host-bind-all', SMS / 'mod.rs', 'empty_webhook_host',
     '#1339 sms oracle: empty-webhook-host'),
    # fix-r1 P2 #15: listen errors keep the io::Error text — the
    # host-unavailable cell's Node EADDRNOTAVAIL line no longer matches.
    ('sms-listen-error-node-text', SMS / 'mod.rs', 'start_host_unavailable',
     '#1339 sms oracle: start-host-unavailable'),
    # #1589 issue #1: the elicitation check and take fall back into two lock
    # sections — a concurrent event-stream clear lands between them, the take
    # finds nothing, and the answer becomes a turn.
    ('sms-elicit-lock-split', SMS / 'mod.rs', 'sms_elicit_lock_split',
     '#1589 elicit lock split must answer the pending elicitation exactly once'),
    # #1589 issue #1: same split for the permission check + beginAnswer — the
    # cleared state reads as "no permission pending" and the answer becomes
    # a turn.
    ('sms-permission-lock-split', SMS / 'mod.rs', 'sms_permission_lock_split',
     '#1589 permission lock split must decide the pending permission exactly once'),
    # #1589 issue #1: the active-chat contains check and the resolve+insert
    # fall back into two lock sections — two concurrent webhooks for one chat
    # both dispatch instead of one.
    ('sms-active-insert-split', SMS / 'mod.rs', 'sms_active_insert_split',
     '#1589 active insert split must dispatch exactly one turn for one chat'),
    # #1589 issue #6: the v4 probe classifies any non-EADDRINUSE failure as a
    # free port (the fix-r5 shape) instead of Unknown — the pure classifier
    # test pins the arm a healthy host never reaches.
    ('sms-v4-probe-unknown-as-unshared', SMS / 'mod.rs',
     'classify_v4_probe_other_errors_stay_unknown',
     '#1589 v4 probe must keep unattributable errors Unknown'),
    # #1729: recording type drift must fail before replay can use defaults.
    ('sms-oracle-recorded-header-map', SMS / 'tests.rs', 'recorded_header_map_rejects_wrong_type',
     '#1339 #1729 rejects headers type drift'),
    ('sms-oracle-recorded-method', SMS / 'tests.rs', 'recorded_method_rejects_wrong_type',
     '#1339 #1729 rejects method type drift'),
    ('sms-oracle-recorded-path', SMS / 'tests.rs', 'recorded_path_rejects_wrong_type',
     '#1339 #1729 rejects path type drift'),
    ('sms-oracle-recorded-body-repeat', SMS / 'tests.rs', 'recorded_body_repeat_rejects_wrong_type',
     '#1339 #1729 rejects body.repeat type drift'),
    ('sms-oracle-recorded-body-count', SMS / 'tests.rs', 'recorded_body_count_rejects_wrong_type',
     '#1339 #1729 rejects body.count type drift'),
    ('sms-oracle-recorded-truncate', SMS / 'tests.rs', 'recorded_truncate_rejects_wrong_type',
     '#1339 #1729 rejects truncate type drift'),
    ('sms-oracle-recorded-delivery', SMS / 'tests.rs', 'recorded_delivery_rejects_wrong_type',
     '#1339 #1729 rejects delivery type drift'),
    ('sms-oracle-recorded-cron-session', SMS / 'tests.rs', 'recorded_cron_session_rejects_wrong_type',
     '#1339 #1729 rejects delivery.sessionKey type drift'),
    ('sms-oracle-recorded-cron-origin-session', SMS / 'tests.rs', 'recorded_cron_origin_session_rejects_wrong_type',
     '#1339 #1729 rejects delivery.originSessionKey type drift'),
    ('sms-oracle-recorded-cron-origin-channel', SMS / 'tests.rs', 'recorded_cron_origin_channel_rejects_wrong_type',
     '#1339 #1729 rejects delivery.originChannelKey type drift'),
    ('sms-oracle-recorded-cron-text', SMS / 'tests.rs', 'recorded_cron_text_rejects_wrong_type',
     '#1339 #1729 rejects delivery.text type drift'),
    ('sms-oracle-recorded-turn', SMS / 'tests.rs', 'recorded_turn_rejects_wrong_type',
     '#1339 #1729 rejects turn type drift'),
    ('sms-oracle-recorded-turn-gate', SMS / 'tests.rs', 'recorded_turn_gate_rejects_wrong_type',
     '#1339 #1729 rejects turn.gate type drift'),
    ('sms-oracle-recorded-turn-error', SMS / 'tests.rs', 'recorded_turn_error_rejects_wrong_type',
     '#1339 #1729 rejects turn.error type drift'),
    ('sms-oracle-recorded-turn-events', SMS / 'tests.rs', 'recorded_turn_events_rejects_wrong_type',
     '#1339 #1729 rejects turn.events type drift'),
    ('sms-oracle-recorded-turn-hold', SMS / 'tests.rs', 'recorded_turn_hold_rejects_wrong_type',
     '#1339 #1729 rejects turn.holdAfter type drift'),
    ('sms-oracle-recorded-permission-error', SMS / 'tests.rs', 'recorded_permission_error_rejects_wrong_type',
     '#1339 #1729 rejects permissionError type drift'),
    ('sms-oracle-recorded-permission-gate', SMS / 'tests.rs', 'recorded_permission_gate_rejects_wrong_type',
     '#1339 #1729 rejects permissionGate type drift'),
    ('sms-oracle-recorded-permission-delivered', SMS / 'tests.rs', 'recorded_permission_delivered_rejects_wrong_type',
     '#1339 #1729 rejects permissionDelivered type drift'),
    ('sms-oracle-recorded-expected', SMS / 'tests.rs', 'recorded_expected_rejects_wrong_type',
     '#1339 #1729 rejects expected type drift'),
    ('sms-oracle-recorded-responses', SMS / 'tests.rs', 'recorded_responses_rejects_wrong_type',
     '#1339 #1729 rejects expected.responses type drift'),
    ('sms-oracle-recorded-env-map', SMS / 'tests.rs', 'recorded_env_map_rejects_wrong_type',
     '#1339 #1729 rejects env type drift'),
    ('sms-oracle-recorded-steps', SMS / 'tests.rs', 'recorded_steps_rejects_wrong_type',
     '#1339 #1729 rejects steps type drift'),
    ('sms-oracle-recorded-half-close', SMS / 'tests.rs', 'recorded_half_close_rejects_wrong_type',
     '#1339 #1729 rejects halfClose type drift'),
    # #1879: both keys are required now, so the switch must kill the
    # presence assertion as well as the type-drift one.
    ('sms-oracle-recorded-log-level', SMS / 'tests.rs',
     ['recorded_log_level_rejects_wrong_type', 'recorded_log_level_rejects_absence'],
     ['#1339 #1729 rejects wait-log.level type drift',
      '#1339 #1879 rejects missing wait-log.level']),
    ('sms-oracle-recorded-log-text', SMS / 'tests.rs',
     ['recorded_log_text_rejects_wrong_type', 'recorded_log_text_rejects_absence'],
     ['#1339 #1729 rejects wait-log.text type drift',
      '#1339 #1879 rejects missing wait-log.text']),
    ('sms-oracle-recorded-sends-count', SMS / 'tests.rs',
     ['recorded_sends_count_rejects_wrong_type', 'recorded_sends_count_rejects_absence'],
     ['#1339 #1729 rejects wait-sends.count type drift',
      '#1339 #1813 wait-sends.count is a required recording field']),
    ('sms-oracle-recorded-rpcs-count', SMS / 'tests.rs',
     ['recorded_rpcs_count_rejects_wrong_type', 'recorded_rpcs_count_rejects_absence'],
     ['#1339 #1729 rejects wait-rpcs.count type drift',
      '#1339 #1813 wait-rpcs.count is a required recording field']),
    ('sms-oracle-recorded-stop-reason', SMS / 'tests.rs', 'recorded_stop_reason_rejects_wrong_type',
     '#1339 #1729 rejects stop.reason type drift'),
    ('sms-oracle-recorded-send-block', SMS / 'tests.rs', 'recorded_send_block_rejects_wrong_type',
     '#1339 #1729 rejects sendBlockAt type drift'),
    ('sms-oracle-recorded-send-fail', SMS / 'tests.rs', 'recorded_send_fail_rejects_wrong_type',
     '#1339 #1729 rejects sendFailAt type drift'),
    ('sms-oracle-recorded-send-error', SMS / 'tests.rs', 'recorded_send_error_rejects_wrong_type',
     '#1339 #1729 rejects sendError type drift'),
    ('sms-oracle-recorded-extra', SMS / 'tests.rs', 'recorded_extra_rejects_wrong_type',
     '#1339 #1729 rejects extra type drift'),
    # #1813: every null-sentinel field must read explicit null as absence, so a
    # hand-edited oracle cannot split one field's convention from its siblings.
    # #1879: the shared switch now restores the old fail-closed read for eleven
    # fields, so the row is an all-cells binding — one test and one assertion
    # per field. A single aggregate test would stop at `permissionDelivered`
    # and never prove that `env` / `sendFailAt` / the four `turn.*` members /
    # `truncate` / `halfClose` / `headers` / `reason` / `extra` were restored
    # at all.
    ('sms-oracle-null-sentinel', SMS / 'tests.rs',
     ['null_sentinels_are_absence_for_every_sentinel_field',
      'recorded_env_map_null_is_absence', 'recorded_send_fail_null_is_absence',
      'null_turn_gate_is_absence', 'null_turn_error_is_absence',
      'null_turn_events_is_absence', 'null_turn_hold_is_absence',
      'recorded_truncate_null_is_absence', 'recorded_half_close_null_is_absence',
      'recorded_header_map_null_is_absence', 'recorded_stop_reason_null_is_absence',
      'recorded_extra_null_is_absence'],
     ['#1339 #1813 null permissionDelivered is absence',
      '#1339 #1879 env null is absence, not type drift',
      '#1339 #1879 sendFailAt null is absence, not type drift',
      '#1339 #1879 null turn.gate is absence',
      '#1339 #1879 null turn.error is absence',
      '#1339 #1879 null turn.events is absence',
      '#1339 #1879 null turn.holdAfter is absence',
      '#1339 #1879 null truncate is absence',
      '#1339 #1879 null halfClose is absence',
      '#1339 #1879 null headers is absence',
      '#1339 #1879 null stop.reason is absence',
      '#1339 #1879 null extra is absence']),
    ('sms-oracle-header-value', SMS / 'tests.rs', 'recorded_header_value_rejects_drift',
     '#1339 #1729 rejects header_value drift'),
    ('sms-oracle-body-type', SMS / 'tests.rs', 'recorded_body_type_rejects_drift',
     '#1339 #1729 rejects body_type drift'),
    ('sms-oracle-body-budget', SMS / 'tests.rs', 'recorded_body_budget_rejects_drift',
     '#1339 #1729 rejects body_budget drift'),
    ('sms-oracle-boundary-body-length', SMS / 'tests.rs', 'recorded_boundary_body_length_rejects_drift',
     '#1339 #1729 rejects boundary_body_length drift'),
    # #1984: the fail-closed branch is disarmed — an undeclared case name whose
    # request body reaches the 1 MiB cap goes back to being skipped silently.
    ('sms-oracle-boundary-unregistered', SMS / 'tests.rs',
     'recorded_boundary_body_length_rejects_unregistered_case',
     '#1339 #1984 rejects an undeclared boundary case'),
    # #2041: build_raw re-derives its wire body length inline (the pre-#2041
    # second copy) and stops honoring `truncate` — the gate and the bytes on
    # the wire fork again, the exact regression the shared derivation closed.
    ('sms-oracle-wire-body-len-fork', SMS / 'tests.rs',
     'gate_and_build_raw_share_the_wire_body_length',
     '#2041 the gate and build_raw must agree on the wire body length'),
    ('sms-oracle-env-value', SMS / 'tests.rs', 'recorded_env_value_rejects_drift',
     '#1339 #1729 rejects env_value drift'),
    ('sms-oracle-send-fail-entry', SMS / 'tests.rs', 'recorded_send_fail_entry_rejects_drift',
     '#1339 #1729 rejects send_fail_entry drift'),
    # #1790: the answer guard is disarmed — a panic, a cancellation or a
    # timeout in the permissionDecide await leaves `answering`/`in_flight` set
    # and the chat is stuck in "权限决定处理中" until restart.
    ('sms-permission-answer-guard-off', SMS / 'permission.rs',
     ['a_panicking_permission_decide_does_not_lock_the_chat',
      'cancelled_permission_decide_does_not_lock_the_chat',
      'timed_out_permission_decide_does_not_lock_the_chat'],
     ['#1790 answering must not survive the permission_decide panic',
      '#1790 answering must not survive cancellation',
      '#1790 answering must not survive the timeout']),
    # #1790: the prompt guard is disarmed — a panic during the confirmation
    # send or the next-prompt send leaves `answering`/`prompt_delivering` set
    # and the chat drops every later message.
    ('sms-permission-prompt-guard-off', SMS / 'permission.rs',
     ['a_panicking_prompt_delivery_does_not_lock_the_chat',
      'a_panicking_confirmation_send_does_not_lock_the_chat',
      'a_permission_captured_during_the_confirmation_send_is_not_dropped',
      'cancelled_confirmation_send_does_not_lock_the_chat',
      'timed_out_confirmation_send_does_not_lock_the_chat'],
     ['#1790 prompt_delivering must not survive the prompt-send panic',
      '#1790 answering must not survive the confirmation-send panic',
      '#1790 the permission captured mid-send must be re-sent after the panic',
      '#1790 answering must not survive the confirmation-send cancellation',
      '#1790 answering must not survive the confirmation-send timeout']),
    # #1901: the sms body-read deadline is disarmed — a client that declares
    # `Content-Length` and stops sending holds the connection task and the
    # socket open forever instead of being answered with Node's 408.
    ('sms-body-read-no-timeout', SMS / 'mod.rs', 'body_read_timeout',
     '#1901 a stalled body is answered at the sms body-read deadline'),
    # #1790: the initial-prompt guard is disarmed — a panic during the first
    # prompt send leaves `initial_prompt_pending` set ("权限提示发送中").
    ('sms-permission-initial-guard-off', SMS / 'permission.rs',
     ['a_panicking_initial_prompt_delivery_does_not_lock_the_chat',
      'cancelled_initial_prompt_delivery_does_not_lock_the_chat'],
     ['#1790 initial_prompt_pending must not survive the prompt-send panic',
      '#1790 initial_prompt_pending must not survive the initial-prompt-send cancellation']),
    # #1957: the `sign: true` reverse constraint only required an object
    # `headers`, so `{}`, a map without the signature key, an empty value, a
    # whitespace-only value, a non-string value, a missing `sign` flag, a signed
    # case with no request step and a second request step without the header all
    # replayed unsigned (`mod.rs` skips the gate on an empty signature) and still
    # matched the recorded 200 — a parity false pass. The
    # switch restores that weaker guard; each cell is judged by its own
    # assertion because one mutation stops the test at its first assertion.
    ('sms-oracle-signed-signature-header', SMS / 'tests.rs',
     ['signed_recording_rejects_empty_headers',
      'signed_recording_rejects_missing_signature_key',
      'signed_recording_rejects_blank_signature_header',
      'signed_recording_rejects_non_boolean_sign',
      'signed_recording_requires_sign_flag',
      'signed_recording_rejects_whitespace_signature_header',
      'signed_recording_rejects_non_string_signature_header',
      'signed_recording_requires_a_request_step',
      'signed_recording_checks_every_request_step'],
     ['#1339 #1957 signed case must record a non-empty X-Twilio-Signature header',
      '#1339 #1957 signed case must record a non-empty X-Twilio-Signature header',
      '#1339 #1957 signed case must record a non-empty X-Twilio-Signature header',
      '#1339 #1957 case sign must be a boolean',
      '#1339 #1957 case must record a sign flag',
      '#1339 #1957 signed case must record a non-empty X-Twilio-Signature header',
      '#1339 #1957 signed case must record a non-empty X-Twilio-Signature header',
      '#1339 #1957 signed case must replay at least one request step',
      '#1339 #1957 signed case must record a non-empty X-Twilio-Signature header']),
    # #1913: the recorded `version` field is read fail-open — a corrupted
    # value replays as the `HTTP/1.1` default and the `http10` cell's
    # close-delimited reply never arrives (the replay times out instead).
    # Killed by the wrong-type rejection, same shape as the other
    # recorded-field switches.
    ('sms-oracle-recorded-version', SMS / 'tests.rs',
     'recorded_version_rejects_wrong_type',
     '#1913 rejects version type drift'),
    # #2152: the recorded `omitHost` field is read fail-open — a corrupted
    # value replays as "keep the Host line" and the missing-host cell sends a
    # Host header, turning the parser-rejection 400 into an admitted routed
    # answer. Killed by the wrong-type rejection, same shape as the other
    # recorded-field switches.
    ('sms-oracle-recorded-omit-host', SMS / 'tests.rs',
     'recorded_omit_host_rejects_wrong_type',
     '#2152 rejects omitHost type drift'),
    # #2691: the preflight's v6 half runs for every host again (the pre-fix
    # over-strict shape) — a v6-only occupier of `[::]:port` that cannot
    # collide with the product's 127.0.0.1 bind fails the pin as a false red.
    ('sms-port-preflight-v6-any-host', SMS / 'tests.rs',
     'v6_only_occupier_does_not_block_the_fixed_127_0_0_1_host',
     '#2691 the v6 half probed the fixed 127.0.0.1 host'),
    # #2691 fix-r3 (review r3, three roles): the post-start bind guard is
    # disarmed — both ownership forms must fail under the switch. The drop
    # form drops the channel before the guard; the replay form keeps it
    # alive (the cron steps), so its threshold is raised by
    # OWNERS_KEPT_ALIVE_ACROSS_START — a start that lost the bind race
    # degrades to the no-op handle again and the pin silently measures the
    # occupier's listener (the pre-fix TOCTOU shape).
    ('sms-pin-bind-race-unchecked', SMS / 'tests.rs',
     ['a_pin_start_that_loses_the_bind_race_is_caught_after_start',
      'a_replay_start_that_keeps_the_channel_alive_is_still_caught_after_start'],
     ['#2691 the post-start bind guard did not fire',
      '#2691 the replay-form post-start bind guard did not fire']),
    # #2691 fix-r3 (review r3 MM boundary#3 / DS concurrency#2 / boundary#3):
    # the fixed-host bind probe is skipped — a dual-stack `[::]:port` holder
    # (the occupier the connect probe cannot see) no longer fails the
    # preflight. The kill no longer leans on Linux-only semantics: on Linux
    # the skipped probe falls through to the refused connect and the bound
    # test's Ok branch proves the product-shaped bind still fails; on macOS
    # the connect stalls on the holder for its whole bounded cap (the
    # coordinator's measured baseline) and the test attributes the stall to
    # the skipped probe with its own anchor. Both verdicts are KILLED;
    # macOS re-verification steps are recorded in the fix-r3 RESULT.
    ('sms-port-preflight-bind-probe-skipped', SMS / 'tests.rs',
     'dual_stack_occupier_is_reported_by_the_fixed_host_bind_probe',
     '#2691 the fixed-host bind probe did not fire'),
]
GUARDED = [SMS / name for name in (
    'mod.rs', 'gateway.rs', 'mapper.rs', 'render.rs', 'elicitation.rs',
    'permission.rs', 'sender.rs', 'tests.rs',
)] + [
    ROOT / 'src/pilotdeck-gateway/src/channel/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml', ROOT / 'src/Cargo.toml', ROOT / 'src/Cargo.lock',
    FIXTURES / '_shared/mutant_switch.py', FIXTURES / 'negative_suite.py', FIXTURES / 'negative_process.py',
    Path(__file__), Path(__file__).with_name('oracle.json'),
    Path(__file__).with_name('generate.mts'), Path(__file__).with_name('README.md'),
]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('names', nargs='*')
    args = parser.parse_args()
    try:
        rows = [dict(zip(('name', 'path', 'test', 'anchor'), row)) for row in switches]
        chosen = mutant_switch.selected(rows, args.names)
        mutant_switch.preflight(rows)
        if args.check:
            if args.output or args.names:
                raise ValueError('--check does not take output or names')
            print(f'{len(rows)} switch anchors and bindings checked')
            return 0
        if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
            raise ValueError('NEW_OUTPUT_DIR and caller CARGO_TARGET_DIR are required')
        output = Path(args.output).resolve()
        output.mkdir(parents=True, exist_ok=False)
        return mutant_switch.run(ROOT, output, chosen,
            mutant_switch.clean_environment(os.environ), guarded=GUARDED,
            prefix=PREFIX, script=Path(__file__), requested=args.names,
            test_timeout=480)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2

if __name__ == '__main__':
    sys.exit(main())

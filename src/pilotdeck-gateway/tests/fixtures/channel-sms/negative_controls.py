#!/usr/bin/env python3
"""#1339 compiled controls: compile every switch once, then run the selected
positive, negative and restored cases with negative_suite verdicts. No source
rewriting, no alternate HEAD and no second target directory. Every switch
mutates exactly one sms-channel behavior and is killed by the oracle replay
cell or dedicated test that pins that behavior's TS bytes (26 → 70: the
fix-r1 round added 11, #1589 added 4, #1729 added 39, #1813 added 1, #1790
added 3, #1901 added 1; the switches table is the truth source). Run one sample in the
authoring session; the coordinator runs the complete list after the patch is
committed.

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
    ('sms-oracle-recorded-log-level', SMS / 'tests.rs', 'recorded_log_level_rejects_wrong_type',
     '#1339 #1729 rejects wait-log.level type drift'),
    ('sms-oracle-recorded-log-text', SMS / 'tests.rs', 'recorded_log_text_rejects_wrong_type',
     '#1339 #1729 rejects wait-log.text type drift'),
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
    ('sms-oracle-null-sentinel', SMS / 'tests.rs', 'null_sentinels_are_absence_for_every_sentinel_field',
     '#1339 #1813 null permissionDelivered is absence'),
    ('sms-oracle-header-value', SMS / 'tests.rs', 'recorded_header_value_rejects_drift',
     '#1339 #1729 rejects header_value drift'),
    ('sms-oracle-body-type', SMS / 'tests.rs', 'recorded_body_type_rejects_drift',
     '#1339 #1729 rejects body_type drift'),
    ('sms-oracle-body-budget', SMS / 'tests.rs', 'recorded_body_budget_rejects_drift',
     '#1339 #1729 rejects body_budget drift'),
    ('sms-oracle-boundary-body-length', SMS / 'tests.rs', 'recorded_boundary_body_length_rejects_drift',
     '#1339 #1729 rejects boundary_body_length drift'),
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

#!/usr/bin/env python3
"""#1334 compiled controls: compile every switch; run the selected positive,
negative and restored cases with negative_suite verdicts. No source rewriting,
no alternate HEAD and no second target directory. Run one sample in the
authoring session; the coordinator can run the complete list after reviewing
the patch.
"""
import argparse
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch
ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
SOURCE_WEBHOOK = ROOT / 'src/pilotdeck-gateway/src/channel/webhook/mod.rs'
SOURCE_HMAC = ROOT / 'src/pilotdeck-gateway/src/channel/webhook/hmac.rs'
SOURCE_CRON = ROOT / 'src/pilotdeck-gateway/src/channel/im_cron.rs'
PREFIX = 'channel::webhook::tests::'
switches = [
    # A GET or OPTIONS on a webhook route must fall through to the plain 404
    # (`get-webhooks` / `options-webhooks` cells), not be handled like a POST.
    ('webhook-route-method-off', SOURCE_WEBHOOK, 'health_and_routing', '#1334 webhook oracle case health-and-routing'),
    # The `sha256=` prefix is optional; keeping it makes the hex decode empty,
    # so the valid `prefixed` and `uppercase` signatures turn into 401s.
    ('webhook-hmac-prefix-off', SOURCE_HMAC, 'invalid_signature', '#1334 webhook oracle case invalid-signature'),
    # Without dedup the `repeat` delivery reaches the gateway twice.
    ('webhook-dedup-off', SOURCE_WEBHOOK, 'duplicate_delivery', '#1334 webhook oracle case duplicate-delivery'),
    # The rate limiter must reject the `third` burst request.
    ('webhook-rate-limit-off', SOURCE_WEBHOOK, 'rate_limit', '#1334 webhook oracle case rate-limit'),
    # extractText tries `text` before `message`; the reversed order answers
    # with the message body instead.
    ('webhook-extract-order', SOURCE_WEBHOOK, 'extract_text_field_order_is_text_first', '#1334 extractText prefers text over message'),
    # A delivery seen exactly at the prune cutoff survives (>=, not >).
    ('webhook-prune-boundary', SOURCE_WEBHOOK, 'prune_keeps_the_exact_cutoff', '#1334 a delivery seen exactly at the cutoff survives pruning'),
    # The rate window slides at exactly 60 s (> cutoff, not >=), so the
    # 160.0 s request is admitted again.
    ('webhook-rate-window', SOURCE_WEBHOOK, 'rate_limit_window_slides_at_sixty_seconds', '#1334 the rate limit window slides at exactly 60 s'),
    # Without the `-{now}` suffix every delivery from one route shares an id.
    ('webhook-delivery-id-fallback', SOURCE_WEBHOOK, 'missing_delivery_id_falls_back_to_route_and_now', '#1334 the delivery id falls back to route and now'),
    # The oversize request must end as a zero-byte close (`closed` cell);
    # answering with the catch arm's 413 instead makes that cell red.
    ('webhook-oversize-answers', SOURCE_WEBHOOK, 'too_large', '#1334 webhook oracle case too-large'),
    # A null JSON body must hang the connection (noResponse cell), never be
    # answered with a 400.
    ('webhook-null-body-responds', SOURCE_WEBHOOK, 'no_text', '#1334 webhook oracle case no-text'),
    # #1534 P0: a pipelined null body must let the client FIN reclaim the
    # connection; without the duplicate-socket FIN probe, stop() hangs forever.
    ('webhook-null-body-probe-off', SOURCE_WEBHOOK, 'pipelined_null_body_disconnect_reclaims_the_connection', '#1334 a pipelined null body reclaims the connection after the client FIN'),
    # The error body carries the TS `code` verbatim, not a decorated one.
    ('webhook-error-body-code', SOURCE_WEBHOOK, 'unknown_route', '#1334 webhook oracle case unknown-route'),
    # `/new` replies with the fixed new-session message.
    ('webhook-new-session-message', SOURCE_WEBHOOK, 'new_command_creates_a_session_and_maps_the_followup', '#1334 /new replies with the new-session message'),
    # The gateway-missing hint is part of the recorded log line.
    ('webhook-gateway-missing-hint', SOURCE_WEBHOOK, 'gateway_missing', '#1334 webhook oracle case gateway-missing'),
    # stop must take the listener slot; keeping it answers the `refused`
    # cell instead of leaving ECONNREFUSED.
    ('webhook-stop-keeps-listener', SOURCE_WEBHOOK, 'stop_and_restart', '#1334 webhook oracle case stop-and-restart'),
    # The body read must carry the requestTimeout deadline; without it a
    # partial body that the client never completes hangs the connection and
    # stop() never returns.
    ('webhook-body-read-no-timeout', SOURCE_WEBHOOK, 'shutdown_with_partial_body_finishes_at_request_timeout', '#1334 the requestTimeout closes the half-open body during stop'),
    # A turn's reply log is truncated at 200 UTF-16 units, not 250.
    ('webhook-log-truncation-200', SOURCE_WEBHOOK, 'submit_and_render', '#1334 webhook oracle case submit-and-render'),
    # A failed-turn log is truncated at 100 UTF-16 units, not 150.
    ('webhook-log-truncation-100', SOURCE_WEBHOOK, 'deliver_other', '#1334 webhook oracle case deliver-other'),
    # #1681: TS gates the extracted text with `if (!text)`, and among strings
    # only `""` is falsy, so an empty `text` field is no text: ignored reply,
    # no deliveryInfo. Accepting it turns the `no-text` empty-string cell red
    # and leaves a deliveryInfo entry behind.
    ('webhook-empty-text-accepted', SOURCE_WEBHOOK,
     ['no_text', 'empty_text_request_is_not_a_delivery'],
     ['#1334 webhook oracle case no-text', '#1681 the empty string is ignored']),
    # #1682: `this.routes[routeName]` walks the prototype chain, so an
    # `Object.prototype` own property name (`constructor`, `__proto__`, ...)
    # resolves to a truthy value with no `secret`/`deliver`: TS authenticates
    # it against the global secret and accepts it. The pre-fix own-key-only
    # lookup answers 404 for every such name, which turns three of the cell's
    # four steps red (`nope-signed` is neither an own key nor a prototype name,
    # so it answers 404 both before and after the fix).
    ('webhook-prototype-routes-off', SOURCE_WEBHOOK, 'object_prototype_route_names',
     '#1334 webhook oracle case object-prototype-route-names'),
    # An `s_` 36-hex session suffix is a cron chat id; without the suffix a
    # cron delivery to the new-session chat resolves no target.
    ('webhook-cron-suffix', SOURCE_CRON, 'cron_delivery_resolves_session_suffix_chat_ids', '#1334 an s_ session key is a cron target'),
    # #1729: recording type drift must fail before replay can use defaults.
    ('webhook-oracle-recorded-method', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_method_rejects_wrong_type',
     '#1334 #1729 rejects method type drift'),
    ('webhook-oracle-recorded-target', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_target_rejects_wrong_type',
     '#1334 #1729 rejects target type drift'),
    ('webhook-oracle-recorded-headers', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_headers_rejects_wrong_type',
     '#1334 #1729 rejects headers type drift'),
    ('webhook-oracle-recorded-body', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_body_rejects_wrong_type',
     '#1334 #1729 rejects body type drift'),
    ('webhook-oracle-recorded-mode', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_mode_rejects_wrong_type',
     '#1334 #1729 rejects mode type drift'),
    # #1684: a recorded request version that is not a string must fail the
    # golden `http10` replay instead of silently sending HTTP/1.1.
    ('webhook-oracle-recorded-version', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_version_rejects_wrong_type',
     '#1334 #1729 rejects version type drift'),
    ('webhook-oracle-recorded-count', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_count_rejects_wrong_type',
     '#1334 #1729 rejects count type drift'),
    ('webhook-oracle-recorded-pin', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_pin_rejects_wrong_type',
     '#1334 #1729 rejects pin type drift'),
    ('webhook-oracle-recorded-cron-session', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_cron_session_rejects_wrong_type',
     '#1334 #1729 rejects cron.sessionKey type drift'),
    ('webhook-oracle-recorded-cron-text', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_cron_text_rejects_wrong_type',
     '#1334 #1729 rejects cron.text type drift'),
    ('webhook-oracle-recorded-cron-origin-session', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_cron_origin_session_rejects_wrong_type',
     '#1334 #1729 rejects cron.originSessionKey type drift'),
    ('webhook-oracle-recorded-cron-origin-channel', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_cron_origin_channel_rejects_wrong_type',
     '#1334 #1729 rejects cron.originChannelKey type drift'),
    ('webhook-oracle-recorded-options', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_options_rejects_wrong_type',
     '#1334 #1729 rejects options type drift'),
    ('webhook-oracle-recorded-port', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_port_rejects_wrong_type',
     '#1334 #1729 rejects options.port type drift'),
    ('webhook-oracle-recorded-routes', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_routes_rejects_wrong_type',
     '#1334 #1729 rejects options.routes type drift'),
    ('webhook-oracle-recorded-secret', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_secret_rejects_wrong_type',
     '#1334 #1729 rejects options.secret type drift'),
    ('webhook-oracle-recorded-rate-limit', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_rate_limit_rejects_wrong_type',
     '#1334 #1729 rejects options.rateLimit type drift'),
    ('webhook-oracle-recorded-max-body', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_max_body_rejects_wrong_type',
     '#1334 #1729 rejects options.maxBodyBytes type drift'),
    ('webhook-oracle-recorded-mapper-uuid', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_mapper_uuid_rejects_wrong_type',
     '#1334 #1729 rejects options.mapperFixedUuid type drift'),
    ('webhook-oracle-recorded-scripts', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_scripts_rejects_wrong_type',
     '#1334 #1729 rejects scripts type drift'),
    ('webhook-oracle-recorded-steps', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_steps_rejects_wrong_type',
     '#1334 #1729 rejects steps type drift'),
    ('webhook-oracle-recorded-op', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_op_rejects_wrong_type',
     '#1334 #1729 rejects op type drift'),
    ('webhook-oracle-recorded-expected', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_expected_rejects_wrong_type',
     '#1334 #1729 rejects expected type drift'),
    ('webhook-oracle-recorded-stop-reason', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_stop_reason_rejects_wrong_type',
     '#1334 #1729 rejects stop.reason type drift'),
    ('webhook-oracle-header-value', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_header_value_rejects_drift',
     '#1334 #1729 rejects header value type drift'),
    ('webhook-oracle-script-entry', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'recorded_script_entry_rejects_drift',
     '#1334 #1729 rejects script entry type drift'),
    # #1813: a zero-step cell replays nothing, so without a whitelist a cell
    # that lost its whole recorded step list would still replay green.
    ('webhook-zero-step-whitelist', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs', 'zero_step_cells_are_the_two_named_cells',
     '#1334 only the named cells may replay zero steps'),
    # #1684: without the request-version finalize, an HTTP/1.0 keep-alive
    # request draws a 1.1 response whose `Connection: close` hyper rewrites to
    # `keep-alive` on an open socket, so the pinned cell turns red.
    ('webhook-http10-finalize-off', SOURCE_WEBHOOK, 'http10_request_closes_the_connection',
     '#1684 pin http10: the 1.0 response closes the connection like Node'),
    # #1684: keeping hyper's known body size adds a `content-length` line to the
    # 1.0 reply where Node is close-delimited, so the recorded header set no
    # longer replays.
    ('webhook-http10-framing-off', SOURCE_WEBHOOK, 'http10_request_closes_the_connection',
     '#1684 pin http10: the 1.0 response replays Node\'s header set'),
    # #2153/#2789: the replay normalization's `content-length: 0` mask is
    # scoped to the `unknown-method` step through `fn parser_admission`. The
    # mask-always mutant makes `normalize` ignore its scope flag, so the
    # injected-line unit test fails; the widening mutant fails the uniqueness
    # guard, which reads the same predicate.
    ('webhook-replay-cl0-mask-always', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs',
     'only_the_registered_step_masks_a_content_length_zero',
     '#2153 an extra content-length: 0 on an unregistered step must not normalize away'),
    ('webhook-replay-cl0-wide', ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs',
     'unknown_method_is_the_label_of_exactly_one_oracle_step',
     "#2153 the unknown-method label belongs to exactly the health-and-routing case's bare parser-level 400"),
]
GUARDED = [SOURCE_WEBHOOK, SOURCE_HMAC, SOURCE_CRON,
    ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/channel/webhook/mapper.rs',
    ROOT / 'src/pilotdeck-gateway/src/channel/webhook/render.rs',
    ROOT / 'src/pilotdeck-gateway/src/channel/webhook/status.rs',
    ROOT / 'src/pilotdeck-gateway/src/channel/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/channel/protocol.rs',
    ROOT / 'src/pilotdeck-gateway/src/native_http.rs',
    ROOT / 'src/pilotdeck-gateway/src/native_http/response.rs',
    ROOT / 'src/pilotdeck-gateway/src/native_http/static_assets.rs',
    ROOT / 'src/pilotdeck-gateway/src/http_proxy.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
    ROOT / 'src/pilotdeck-gateway/tests/common/negative_receipt.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml', ROOT / 'src/Cargo.toml', ROOT / 'src/Cargo.lock',
    FIXTURES / '_shared/mutant_switch.py', FIXTURES / 'negative_suite.py', FIXTURES / 'negative_process.py',
    FIXTURES / 'negative_panic.py', FIXTURES / 'anchor_selfcheck.py',
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
            prefix=PREFIX, script=Path(__file__), requested=args.names)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2

if __name__ == '__main__':
    sys.exit(main())

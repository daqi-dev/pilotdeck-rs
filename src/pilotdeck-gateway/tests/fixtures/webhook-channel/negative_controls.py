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
    # A null JSON body must hang the connection (noResponse cell), never be
    # answered with a 400.
    ('webhook-null-body-responds', SOURCE_WEBHOOK, 'no_text', '#1334 webhook oracle case no-text'),
    # The error body carries the TS `code` verbatim, not a decorated one.
    ('webhook-error-body-code', SOURCE_WEBHOOK, 'unknown_route', '#1334 webhook oracle case unknown-route'),
    # `/new` replies with the fixed new-session message.
    ('webhook-new-session-message', SOURCE_WEBHOOK, 'new_command_creates_a_session_and_maps_the_followup', '#1334 /new replies with the new-session message'),
    # The gateway-missing hint is part of the recorded log line.
    ('webhook-gateway-missing-hint', SOURCE_WEBHOOK, 'gateway_missing', '#1334 webhook oracle case gateway-missing'),
    # stop must take the listener slot; keeping it answers the `refused`
    # cell instead of leaving ECONNREFUSED.
    ('webhook-stop-keeps-listener', SOURCE_WEBHOOK, 'stop_and_restart', '#1334 webhook oracle case stop-and-restart'),
    # A turn's reply log is truncated at 200 UTF-16 units, not 250.
    ('webhook-log-truncation-200', SOURCE_WEBHOOK, 'submit_and_render', '#1334 webhook oracle case submit-and-render'),
    # A failed-turn log is truncated at 100 UTF-16 units, not 150.
    ('webhook-log-truncation-100', SOURCE_WEBHOOK, 'deliver_other', '#1334 webhook oracle case deliver-other'),
    # An `s_` 36-hex session suffix is a cron chat id; without the suffix a
    # cron delivery to the new-session chat resolves no target.
    ('webhook-cron-suffix', SOURCE_CRON, 'cron_delivery_resolves_session_suffix_chat_ids', '#1334 an s_ session key is a cron target'),
]
GUARDED = [SOURCE_WEBHOOK, SOURCE_HMAC, SOURCE_CRON,
    ROOT / 'src/pilotdeck-gateway/src/channel/webhook/tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/channel/webhook/mapper.rs',
    ROOT / 'src/pilotdeck-gateway/src/channel/webhook/render.rs',
    ROOT / 'src/pilotdeck-gateway/src/channel/webhook/status.rs',
    ROOT / 'src/pilotdeck-gateway/src/channel/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/channel/protocol.rs',
    ROOT / 'src/pilotdeck-gateway/src/native_http.rs',
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

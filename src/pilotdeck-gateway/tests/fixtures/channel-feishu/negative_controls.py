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

#!/usr/bin/env python3
"""#1344 compiled controls: compile every switch once, then run the selected
positive, negative and restored cases with negative_suite verdicts. No source
rewriting, no alternate HEAD and no second target directory. Every switch
mutates exactly one slack-channel behavior and is killed by the oracle replay
cell or dedicated test that pins that behavior's TS bytes (the `switches`
table is the truth source; 11 switches — nine behavioral sites in `mod.rs`,
one mapper command gate, one render fragment gate). Run one sample in the
authoring session; the coordinator runs the complete list after the patch is
committed.

The replay-bound rows are judged by the FIRST case the 35-case sequential
replay reaches that the switch changes — the case named in each anchor is
that first cell, verified live per switch (e.g. `slack-mention-strip-off`
first fails at `mention-only-no-turn`, before `mention-stripped`, because the
un-stripped mention-only text is no longer empty and dispatches a turn;
`slack-mrkdwn-off` is bound to the direct conversion unit test because the
replay's first markdown-bearing reply is the `elicitation-answer` prompt).
"""
import argparse
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch
ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
SLACK = ROOT / 'src/pilotdeck-gateway/src/channel/slack'
PREFIX = 'channel::slack::tests::'
switches = [
    # bot_id / bot_message discriminator off: the bot-id-filtered cell's
    # message is dispatched instead of silently dropped.
    ('slack-bot-id-filter-off', SLACK / 'mod.rs', 'channel_matches_real_ts',
     '#1344 case bot-id-filtered matches the TS oracle'),
    # mention strip off: the mention-only cell's text is no longer empty, so
    # it dispatches a turn where the oracle records none.
    ('slack-mention-strip-off', SLACK / 'mod.rs', 'channel_matches_real_ts',
     '#1344 case mention-only-no-turn matches the TS oracle'),
    # thread-qualified chat id off: the thread-reply cell's submitTurn carries
    # the bare channel id instead of `C1:<thread_ts>`.
    ('slack-thread-key-off', SLACK / 'mod.rs', 'channel_matches_real_ts',
     '#1344 case thread-reply matches the TS oracle'),
    # active-chat admission off: the active-skip cell starts a second turn
    # instead of the "处理中" status reply.
    ('slack-active-dedupe-off', SLACK / 'mod.rs', 'channel_matches_real_ts',
     '#1344 case active-skip matches the TS oracle'),
    # turn-error reply text replaced: the turn-error cell's user-facing
    # failure text no longer matches TS.
    ('slack-error-reply-text', SLACK / 'mod.rs', 'channel_matches_real_ts',
     '#1344 case turn-error matches the TS oracle'),
    # post-stop send guard off: the stop-during-turn cell's reply is posted
    # after `app` is `null` where the oracle records no postMessage.
    ('slack-stopped-send-still-sends', SLACK / 'mod.rs', 'channel_matches_real_ts',
     '#1344 case stop-during-turn matches the TS oracle'),
    # postMessage thread_ts dropped: the thread-reply cell's reply leaves the
    # thread and lands on the channel root.
    ('slack-post-message-thread-off', SLACK / 'mod.rs', 'channel_matches_real_ts',
     '#1344 case thread-reply matches the TS oracle'),
    # formatSlackMrkdwn off: the direct conversion unit test keeps `**b**`
    # and `[t](u)` unconverted (the replay's first markdown-bearing reply is
    # the elicitation-answer prompt's `*选项*`).
    ('slack-mrkdwn-off', SLACK / 'mod.rs', 'mrkdwn_conversion_bold_then_link',
     '#1344 case mrkdwn-and-link-formats matches the TS oracle'),
    # chunking off: the direct split unit test returns one segment where TS
    # splits at the last newline past halfway.
    ('slack-chunk-off', SLACK / 'mod.rs', 'chunk_text_splits_at_the_last_newline_past_halfway',
     '#1344 case long-text-chunked matches the TS oracle'),
    # /new command gate off: the new-session cell keeps the general session
    # key instead of minting the s_ uuid session.
    ('slack-mapper-new-command', SLACK / 'mapper.rs',
     'mapper_new_command_switches_to_a_fresh_session',
     '#1344 the /new command switches the chat to a fresh s_ session'),
    # tool-failure fragment dropped: the failed tool call's warning line
    # "⚠️ <name> failed" disappears from the rendered body.
    ('slack-render-failure-fragment', SLACK / 'render.rs',
     'render_reports_a_failed_tool_call_by_name',
     '#1344 a failed tool call renders its warning line'),
]
GUARDED = [SLACK / name for name in ('mod.rs', 'mapper.rs', 'render.rs', 'tests.rs')] + [
    ROOT / 'src/pilotdeck-gateway/src/channel/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml', ROOT / 'src/Cargo.toml', ROOT / 'src/Cargo.lock',
    FIXTURES / '_shared/mutant_switch.py', FIXTURES / 'negative_suite.py', FIXTURES / 'negative_process.py',
    Path(__file__), Path(__file__).with_name('oracle.json'),
    Path(__file__).with_name('generate.mts'), Path(__file__).with_name('README.md'),
    Path(__file__).with_name('bolt-stub.mjs'),
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

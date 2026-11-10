#!/usr/bin/env python3
"""#1344 compiled controls: compile every switch once, then run the selected
positive, negative and restored cases with negative_suite verdicts. No source
rewriting, no alternate HEAD and no second target directory. Every switch
mutates exactly one slack-channel behavior and is killed by the oracle replay
cell or dedicated test that pins that behavior's TS bytes (the `switches`
table is the truth source; 14 switches — twelve behavioral sites in `mod.rs`,
one mapper command gate, one render fragment gate). Run one sample in the
authoring session; the coordinator runs the complete list after the patch is
committed.

The replay-bound rows are judged by the FIRST case the 36-case sequential
replay reaches that the switch changes — the case named in each anchor is
that first cell, verified live per switch (e.g. `slack-mention-strip-off`
first fails at `mention-only-no-turn`, before `mention-stripped`, because the
un-stripped mention-only text is no longer empty and dispatches a turn;
`slack-mrkdwn-off` is bound to the direct conversion unit test because the
replay's first markdown-bearing reply is the `elicitation-answer` prompt).

`--oracle-integrity` (#2311) runs the source-patch controls that protect the
oracle itself, the bluebubbles precedent's three: a shrunk case list, a
wrong `expectedCaseCount` declaration and an empty cases array each have to
trip the dedicated pin, never silently replay fewer cells. They run through
negative_suite's matrix_main (source-patch form — the slack fixture owns the
rows; `_shared/mutant_switch.py` stays #1099 M6-exclusive). The full matrix
run is post-merge (matrix_main refuses uncommitted product files); the
authoring session verifies each row by hand: apply, `cargo test --no-run`,
run the bound test, observe the red, byte-restore.
"""
import argparse
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch
from negative_suite import matrix_main
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
    # #2313: the per-frame spawn race restored — frame B's admission task can
    # overtake frame A's continuation, so the back-to-back cell's first frame
    # is swallowed as already active where the TS oracle records its turn.
    ('slack-frame-admission-race', SLACK / 'mod.rs', 'channel_matches_real_ts',
     '#1344 case back-to-back-frames matches the TS oracle'),
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
    # #2321: the pre-fix default restored — the default base carries its own
    # `/api`, so every REST call joins `https://slack.com/api/api/...`.
    ('slack-default-base-double-api', SLACK / 'mod.rs',
     'default_rest_base_joins_the_contract_paths_once',
     '#2321 the default REST base joins the contract paths once'),
    # #2321: abnormal-close logging off — a 1011 close frame leaves no
    # bolt error in the log.
    ('slack-abnormal-close-silent', SLACK / 'mod.rs',
     'abnormal_close_logs_the_stub_bolt_error',
     '#2321 an abnormal server close logs the stub bolt error'),
]

# #2311: the source-patch oracle-integrity controls, the bluebubbles
# precedent's three, bound to the golden replay and its dedicated oracle pins
# in `tests.rs`.
ORACLE_MUTATIONS = [
    # Dropping a case from the parsed oracle must trip the generator's
    # literal 36, never silently pass with fewer cells.
    ('oracle-shrunk-list', SLACK / 'tests.rs',
     '    let oracle = oracle();\n    for case in cases(&oracle) {\n',
     '    let mut oracle = oracle();\n'
     '    let _ = oracle["cases"].as_array_mut().expect("#1344 mutant cases").pop();\n'
     '    oracle["expectedCaseCount"] = json!(35);\n'
     '    for case in cases(&oracle) {\n',
     'slack::tests::channel_matches_real_ts',
     "#1344 oracle holds the generator's 36 cases"),
    # A wrong expectedCaseCount declaration must trip the count pin, never
    # replay the cases it disagrees with.
    ('oracle-wrong-declaration', Path(__file__).with_name('oracle.json'),
     '"expectedCaseCount": 36,',
     '"expectedCaseCount": 35,',
     'slack::tests::channel_matches_real_ts',
     '#1344 oracle case count matches its declaration'),
    # An empty cases array must be rejected, never skipped.
    ('oracle-empty-cases', Path(__file__).with_name('oracle.json'),
     '  "cases": [\n',
     '  "cases": [],\n  "disabledCases": [\n',
     'slack::tests::channel_matches_real_ts',
     '#1344 oracle cases non-empty'),
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
    parser.add_argument('--check-anchors', action='store_true')
    parser.add_argument('--oracle-integrity', action='store_true',
                        help='run the #2311 source-patch oracle-integrity matrix '
                             'instead of the compiled switches')
    parser.add_argument('output', nargs='?')
    parser.add_argument('names', nargs='*')
    args = parser.parse_args()
    try:
        if args.oracle_integrity:
            if args.check:
                raise ValueError('--check is the compiled-switch anchor check; the '
                                 'oracle-integrity matrix takes --check-anchors')
            if args.check_anchors:
                if args.output or args.names:
                    raise ValueError('--check-anchors does not take output or names')
                return matrix_main(ROOT, __file__, 'channel::', ORACLE_MUTATIONS,
                                   passed_count=1, child_count=0, paths=GUARDED,
                                   exact=True, args=['--check'])
            if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
                raise ValueError('NEW_OUTPUT_DIR and caller CARGO_TARGET_DIR are required')
            return matrix_main(ROOT, __file__, 'channel::', ORACLE_MUTATIONS,
                               passed_count=1, child_count=0, paths=GUARDED,
                               exact=True, args=[args.output, *args.names])
        if args.check_anchors:
            raise ValueError('--check-anchors belongs to --oracle-integrity')
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

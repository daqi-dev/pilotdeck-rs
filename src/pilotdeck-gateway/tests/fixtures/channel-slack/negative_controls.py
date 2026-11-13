#!/usr/bin/env python3
"""#1344 compiled controls: compile every switch once, then run the selected
positive, negative and restored cases with negative_suite verdicts. No source
rewriting, no alternate HEAD and no second target directory. Every switch
mutates exactly one slack-channel behavior and is killed by the oracle replay
cell or dedicated test that pins that behavior's TS bytes (the `switches`
table is the truth source; 14 switches — twelve behavioral sites in `mod.rs`,
one mapper command gate, one render fragment gate). The #2311 oracle
integrity controls and the #2312 failed-auth.test session release live in
the `MUTATIONS` table — the same routine run, in the bluebubbles precedent's
source-patch form through negative_suite's matrix_main (`_shared/
mutant_switch.py` stays #1099 M6-exclusive). Run one sample in the authoring
session; the coordinator runs the complete list after the patch is committed.

The replay-bound rows are judged by the FIRST case the 36-case sequential
replay reaches that the switch changes — the case named in each anchor is
that first cell, verified live per switch (e.g. `slack-mention-strip-off`
first fails at `mention-only-no-turn`, before `mention-stripped`, because the
un-stripped mention-only text is no longer empty and dispatches a turn;
`slack-mrkdwn-off` is bound to the direct conversion unit test because the
replay's first markdown-bearing reply is the `elicitation-answer` prompt).

The routine run is two sequential stages in one output directory: the
compiled switches first (`mutant_switch.run` — the `switches` table), then
the source patches (`matrix_main` — `MUTATIONS`, evidence under `<output>/
source-patch`). Both stages take the fixtures tree lock, so they cannot
overlap; either stage failing fails the run, and a name selects into the
table that declares it. `--check` and `--check-anchors` are one check
covering both surfaces — the compiled switch sites and bindings, and the
source-patch anchors — and both read surfaces now run inside the shared
`anchor_read_window` device: a busy tree lock or a failed read is a refusal
to start (rc 2), never an unlocked read of a sibling runner's mutant. The
full matrix run is post-merge (matrix_main refuses uncommitted product
files); the authoring session verifies each row by hand:
apply, `cargo test --no-run`, run the bound test, observe the red,
byte-restore.
"""
import argparse
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch
from negative_process import anchor_read_window
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

# The source-patch rows the routine run judges after the compiled switches:
# the #2311 oracle-integrity controls (the bluebubbles precedent's three,
# bound to the golden replay and its dedicated oracle pins in `tests.rs`)
# and the #2312 failed-auth.test session release (bound to its dedicated
# release pin). The table name is one anchor_selfcheck's TABLES recognizes,
# so the anchor gate covers these rows the way the switches gate covers the
# compiled table.
MUTATIONS = [
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
    # #2312: the failed-auth.test session release dropped — the ok:false path
    # keeps the session resident (reader task + `Arc<SlackShared>` clone), so
    # the shared state outlives the channel and the dedicated release pin
    # fails on its anchor.
    ('slack-auth-failure-abandon-off', SLACK / 'mod.rs',
     '    if !super::sms::truthy(&auth.get("ok").cloned().unwrap_or(JsValue::Undefined)) {\n'
     '        abandon_session(shared).await;\n',
     '    if !super::sms::truthy(&auth.get("ok").cloned().unwrap_or(JsValue::Undefined)) {\n',
     'slack::tests::auth_test_failure_releases_the_session',
     '#2312 a failed auth.test releases the session'),
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
    parser.add_argument('--check', action='store_true',
                        help='check both surfaces: the compiled switch sites '
                             'and bindings, and the source-patch anchors')
    parser.add_argument('--check-anchors', action='store_true',
                        help='the same check; kept for the fixture convention '
                             'that spells it --check-anchors')
    parser.add_argument('output', nargs='?')
    parser.add_argument('names', nargs='*')
    args = parser.parse_args()
    try:
        rows = [dict(zip(('name', 'path', 'test', 'anchor'), row)) for row in switches]
        switch_names = [name for name in args.names if any(name == row[0] for row in switches)]
        patch_names = [name for name in args.names if any(name == row[0] for row in MUTATIONS)]
        if len(switch_names) + len(patch_names) != len(args.names):
            unknown = [name for name in args.names
                       if name not in {row[0] for row in switches} | {row[0] for row in MUTATIONS}]
            raise ValueError(f'unknown control name(s): {", ".join(unknown)}')
        chosen = mutant_switch.selected(rows, switch_names)
        if args.check or args.check_anchors:
            if args.output or args.names:
                raise ValueError('--check does not take output or names')
            # #2795 fix-r5（评审 sibling P1#3 + simplify P2#1 结构性收口）：
            # --check 的读面此前是裸读——mutant_switch.preflight 逐行读 SLACK
            # 源、matrix_main 的 --check 分支逐行读锚点源，兄弟变异窗口中间的
            # 变异体会被核对错在它不拥有的源上。两段都改走共享
            # anchor_read_window 装置（两个顺序锁窗口，无嵌套），锁忙 / 读失败
            # 是拒跑（2）。run 模式的 preflight 保持原样：后续
            # mutant_switch.run 自持树锁并做 pristine 校验。
            with anchor_read_window(ROOT, 'channel-slack --check'):
                mutant_switch.preflight(rows)
            print(f'{len(rows)} switch anchors and bindings checked')
            return matrix_main(ROOT, __file__, 'channel::', MUTATIONS,
                               passed_count=1, child_count=0, paths=GUARDED,
                               exact=True, args=['--check'])
        # #2795 fix-r6（评审 sibling#2）：run 分支的 preflight 此前保持裸读——
        # mutant_switch.run 的 pristine 校验在 run 内部（取锁之后），preflight
        # 先读仍可能被兄弟变异 write/restore 窗口打成假拒跑（stale ValueError
        # → 外层 catch → rc 2 `preflight:`，合法实跑被误归因）。与 --check 分支
        # 同形包进共享装置：锁忙/读失败是拒跑（SystemExit(2) 不进外层
        # ValueError/OSError catch），与后续 mutant_switch.run 顺序取锁、无嵌套。
        with anchor_read_window(ROOT, 'channel-slack preflight'):
            mutant_switch.preflight(rows)
        if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
            raise ValueError('NEW_OUTPUT_DIR and caller CARGO_TARGET_DIR are required')
        output = Path(args.output).resolve()
        output.mkdir(parents=True, exist_ok=False)
        if not args.names or switch_names:
            # Stage one, the compiled switches; `chosen` is every switch when
            # no names were requested, else the requested switch subset.
            code = mutant_switch.run(ROOT, output, chosen,
                mutant_switch.clean_environment(os.environ), guarded=GUARDED,
                prefix=PREFIX, script=Path(__file__), requested=switch_names,
                test_timeout=480)
            if code:
                return code
        if not args.names or patch_names:
            # Stage two, the source patches, in their own evidence subdir:
            # matrix_main claims an output directory that does not exist yet.
            return matrix_main(ROOT, __file__, 'channel::', MUTATIONS,
                               passed_count=1, child_count=0, paths=GUARDED,
                               exact=True,
                               args=[str(output / 'source-patch'), *patch_names])
        return 0
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2

if __name__ == '__main__':
    sys.exit(main())

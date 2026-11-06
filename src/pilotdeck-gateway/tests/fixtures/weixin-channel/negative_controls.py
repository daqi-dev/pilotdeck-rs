#!/usr/bin/env python3
"""#1328 HEAD-only weixin channel controls; compile each selected mutant before judging it.

OUT [mutation ...], --compile-only OUT, --check (also --check-anchors).
The shared runner owns source restoration, offline cargo, nonce receipts,
patch evidence, pending registrations and the complete result summary.

Killers live in three module roots (methods tests, the channel replay, the
oracle guards inside the replay), so every row carries a fully qualified
libtest name against an empty matrix prefix; each row is exact anyway.
"""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_suite import matrix_main

root = Path(__file__).resolve().parents[5]
fixtures = Path(__file__).resolve().parents[1]
weixin = root / 'src/pilotdeck-gateway/src/channel/weixin'
native = root / 'src/pilotdeck-gateway/src/native_process'
REPLAY = 'channel::weixin::replay_tests::weixin_replays_the_ts_channel_oracle'
UNSUPPORTED = ('native_process::methods::tests::'
               'prepare_weixin_login_without_callback_answers_unsupported')
FORWARDED = ('native_process::methods::tests::'
             'prepare_weixin_login_forwards_the_callback_result_each_call')
MUTATIONS = [
    ('unsupported-reason', native / 'methods/weixin_login.rs',
     '("reason", "unsupported".into()),',
     '("reason", "disabled".into()),',
     UNSUPPORTED, '#1328 unsupported shape reason'),
    ('unsupported-requested-flag', native / 'methods/weixin_login.rs',
     '("requested", JsValue::Bool(false)),',
     '("requested", JsValue::Bool(true)),',
     UNSUPPORTED, '#1328 unsupported shape requested flag'),
    ('callback-not-forwarded', native / 'methods/weixin_login.rs',
     'Some(prepare) => prepare().await,',
     'Some(_) => Ok(object([("requested", JsValue::Bool(true))])),',
     FORWARDED, '#1328 callback result requestedAt is forwarded verbatim'),
    ('callback-requested-flag', native / 'channels.rs',
     '("requested", JsValue::Bool(true)),',
     '("requested", JsValue::Bool(false)),',
     REPLAY, '#1328 weixin replay oracle'),
    ('chunk-length', weixin / 'mod.rs',
     '.send_text_chunked(user_id, text, &context_token, 2000)',
     '.send_text_chunked(user_id, text, &context_token, 4000)',
     REPLAY, '#1328 weixin harness gate did not resolve'),
    ('new-session-prefix', weixin / 'mapper.rs',
     'units.extend_from_slice("s_".encode_utf16().collect::<Vec<_>>().as_slice());',
     'units.extend_from_slice("x_".encode_utf16().collect::<Vec<_>>().as_slice());',
     REPLAY, '#1328 weixin replay oracle'),
    ('login-failure-message', weixin / 'client.rs',
     '"Login failed: server did not return required credentials"',
     '"Login failed: required credentials missing"',
     REPLAY, '#1328 weixin replay oracle'),
    ('credentials-fallback', weixin / 'client.rs',
     'let (Some(bot_token), Some(account_id)) = (bot_token, account_id) else {',
     'let (Some(bot_token), Some(account_id)) = '
     '(bot_token.clone().or(account_id.clone()), account_id.or(bot_token)) else {',
     REPLAY, '#1328 weixin harness gate did not resolve'),
    ('oracle-empty-cases', fixtures / 'weixin-channel/oracle.json',
     '  "cases": [\n',
     '  "cases": [],\n  "disabledCases": [\n',
     REPLAY, '#1328 weixin oracle cases must be non-empty'),
    ('replay-missing-case', weixin / 'replay_tests.rs',
     '    for case in oracle_cases(&oracle) {',
     '    let mut trimmed = oracle.clone();\n'
     '    let _ = trimmed["cases"].as_array_mut().expect("#1328 mutant cases").pop();\n'
     '    for case in oracle_cases(&trimmed) {',
     REPLAY, '#1328 weixin oracle case count'),
    ('oracle-wrong-declaration', fixtures / 'weixin-channel/oracle.json',
     '  "expectedCaseCount": 15,',
     '  "expectedCaseCount": 14,',
     REPLAY, '#1328 weixin generator declaration'),
    # JS truthiness (`if (x)` is false for ""): each of the three filters has
    # its own golden case, so dropping the filter must turn the replay red.
    ('empty-buffer-clears-cursor', weixin / 'client.rs',
     'string_field(&response, "get_updates_buf").filter(|b| !b.units().is_empty())',
     'string_field(&response, "get_updates_buf")',
     REPLAY, '#1328 weixin replay oracle'),
    ('empty-ticket-sends-typing', weixin / 'client.rs',
     'string_field(&config, "typing_ticket").filter(|ticket| !ticket.units().is_empty())',
     'string_field(&config, "typing_ticket")',
     REPLAY, '#1328 weixin replay oracle'),
    ('empty-bot-token-accepted', weixin / 'client.rs',
     '                let bot_token =\n'
     '                    string_field(&status, "bot_token").filter(|token| '
     '!token.units().is_empty());\n'
     '                let account_id =\n'
     '                    string_field(&status, "ilink_bot_id").filter(|id| '
     '!id.units().is_empty());',
     '                let bot_token = string_field(&status, "bot_token");\n'
     '                let account_id = string_field(&status, "ilink_bot_id");',
     # With the filters gone the mutant accepts the empty token, so c12 never
     # reports `failed`: the replay dies on the harness gate (10 s) before it
     # can reach the oracle comparison — same kill shape as `credentials-fallback`.
     REPLAY, '#1328 weixin harness gate did not resolve'),
]


def main():
    guarded = list(weixin.glob('*.rs'))
    guarded += [native / 'channels.rs', native / 'methods/weixin_login.rs',
                root / 'src/pilotdeck-gateway/tests/weixin_channel.rs']
    guarded.extend(path for path in (fixtures / 'weixin-channel').rglob('*')
                  if path.is_file())
    args = ['--check'] if sys.argv[1:] == ['--check-anchors'] else sys.argv[1:]
    return matrix_main(root, __file__, '', MUTATIONS,
                       passed_count=1, child_count=0, paths=guarded,
                       exact=True, args=args)


if __name__ == '__main__':
    raise SystemExit(main())

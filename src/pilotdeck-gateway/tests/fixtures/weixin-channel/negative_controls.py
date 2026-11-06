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
MEDIA_DECRYPT = 'channel::weixin::media::tests::decrypt_replays_the_oracle_media_cases'
MEDIA_DOWNLOAD = 'channel::weixin::media::tests::download_replays_the_oracle_cases'
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
    # JS truthiness (`if (x)` is false for ""): four filters, four golden cases,
    # one row per filter so every case has its own killer. Death points differ:
    # c14/c15 die on the oracle comparison (`#1328 weixin replay oracle`); the
    # credential pair dies earlier — the empty value is accepted, the `failed`
    # report never arrives, and the replay hits the 10 s harness gate
    # (`#1328 weixin harness gate did not resolve`, same shape as
    # `credentials-fallback`).
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
     '!token.units().is_empty());',
     '                let bot_token = string_field(&status, "bot_token");',
     REPLAY, '#1328 weixin harness gate did not resolve'),
    ('empty-account-id-accepted', weixin / 'client.rs',
     '                let account_id =\n'
     '                    string_field(&status, "ilink_bot_id").filter(|id| '
     '!id.units().is_empty());',
     '                let account_id = string_field(&status, "ilink_bot_id");',
     REPLAY, '#1328 weixin harness gate did not resolve'),
    # #1821 live-reply controller mutants (controller-level mock tests and the
    # weixin watchdog integration tests in channel/weixin/tests.rs; each row
    # binds every named killer cell, exact mode requires all of them to fail).
    ('live-reply-edit-dropped', weixin / 'live_reply.rs',
     'if self.current_segment.handle.is_some() && self.transport.edit.is_some() {',
     'if self.current_segment.handle.is_some() && self.transport.edit.is_none() {',
     [('channel::weixin::live_reply::tests::'
       'edit_intermediate_updates_follow_the_ts_request_sequence',
       '#1821 edit intermediate updates follow the ts request sequence')]),
    ('live-reply-watchdog-not-armed', weixin / 'live_reply.rs',
     'self.turn_timer =\n'
     '            Some(Instant::now() + Duration::from_millis(self.options.turn_timeout_ms));',
     'self.turn_timer = None;',
     [('channel::weixin::live_reply::tests::'
       'turn_timeout_marks_timed_out_and_reports_the_timer',
       '#1821 turn timeout watchdog fires '
       'turn-timeout-marks-timed-out-and-reports-the-timer'),
      ('channel::weixin::tests::turn_timeout_watchdog_aborts_with_system_timeout_reason',
       '#1821 weixin turn timeout watchdog aborts with the system timeout reason')]),
    ('live-reply-activity-not-pulsed', weixin / 'live_reply.rs',
     'if let Some(pulse) = self.transport.pulse_activity.clone() {',
     'if let Some(pulse) = self.transport.pulse_activity.clone().filter(|_| false) {',
     [('channel::weixin::live_reply::tests::'
       'activity_pulses_fire_after_the_delay_and_respect_max_updates',
       '#1821 activity pulses must fire '
       'activity-pulses-fire-after-the-delay-and-respect-max-updates'),
      ('channel::weixin::live_reply::tests::'
       'activity_delay_is_silent_just_before_ten_minutes',
       '#1821 activity pulses must fire '
       'activity-delay-is-silent-just-before-ten-minutes')]),

    # #1808: the terminal comparison must reject a wrong-typed `expected`
    # instead of falling back to the old blind read.
    ('weixin-recorded-expected-wrong-type', weixin / 'replay_tests.rs',
     '        Some(expected) => Err(format!("#1328 expected must be an object, got {expected}")),',
     '        Some(expected) => Ok(expected),',
     'channel::weixin::replay_tests::recorded_expected_rejects_wrong_type',
     '#1328 #1808 rejects expected type drift'),
    # #1808: the absent arm is a separate guard; regress it to the old blind
    # read (fall back to the case itself) and the same plain assertion dies.
    ('weixin-recorded-expected-absent', weixin / 'replay_tests.rs',
     '        None => Err("#1328 expected must be an object, got <missing>".into()),',
     '        None => Ok(value),',
     'channel::weixin::replay_tests::recorded_expected_rejects_wrong_type',
     '#1328 #1808 rejects absent expected'),

    # #1823 inbound media. The decrypt rows are killed by the media oracle
    # replay (`media-file-plaintext-wins-over-decrypt` exists precisely so the
    # plaintext short-circuit is observable: that buffer's own decryption is
    # *also* a known file, so skipping the early return adopts the decoy).
    ('media-plaintext-passthrough-dropped', weixin / 'media.rs',
     '    if is_expected(buffer) {\n        return None;\n    }',
     '    if is_expected(buffer) && buffer.is_empty() {\n        return None;\n    }',
     MEDIA_DECRYPT, '#1823 media-file-plaintext-wins-over-decrypt file changed'),
    ('media-candidates-first-key-only', weixin / 'media.rs',
     '    for key in extract_weixin_media_keys(media_owner) {\n'
     '        for deciphered in try_decrypt_weixin_media(buffer, &key) {',
     '    for key in extract_weixin_media_keys(media_owner).into_iter().take(1) {\n'
     '        for deciphered in try_decrypt_weixin_media(buffer, &key) {',
     MEDIA_DECRYPT, '#1823 media-image-second-candidate-hits result'),
    ('media-key-length-always-128', weixin / 'media.rs',
     '        24 => decrypt_blocks::<Aes192>(key, &mut padded),',
     '        24 => decrypt_blocks::<Aes128>(key, &mut padded),',
     MEDIA_DECRYPT, '#1823 media-image-media-base64-aes192 result'),
    ('media-all-miss-panics', weixin / 'media.rs',
     '            if is_expected(&deciphered) {\n'
     '                return Some(deciphered);\n'
     '            }\n'
     '        }\n'
     '    }\n'
     '    None\n',
     '            if is_expected(&deciphered) {\n'
     '                return Some(deciphered);\n'
     '            }\n'
     '        }\n'
     '    }\n'
     '    panic!("#1823 no candidate matched")\n',
     MEDIA_DECRYPT, '#1823 media-image-all-miss-falls-back'),
    ('media-pkcs7-strip-dropped', weixin / 'media.rs',
     '    let stripped = strip_pkcs7_padding(&padded).map(<[u8]>::to_vec);\n'
     '    outputs.push(stripped.unwrap_or(padded));',
     '    outputs.push(padded);',
     MEDIA_DECRYPT, '#1823 media-image-owner-hex-aes128 result'),
    # The trailing-single-character guard is what keeps `chunk[1]` in range;
    # `node_base64_semantics` feeds "A" and "!".
    ('media-base64-single-char-guard', weixin / 'media.rs',
     '        if chunk.len() < 2 {\n            break;\n        }',
     '        if chunk.len() < 2 && chunk.is_empty() {\n            break;\n        }',
     'channel::weixin::media::tests::node_base64_semantics',
     '#1823 base64 decode'),
    ('media-download-redirect-not-followed', weixin / 'media.rs',
     '                if matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)',
     '                if matches!(status.as_u16(), 301 | 303 | 307 | 308)',
     MEDIA_DOWNLOAD, '#1823 media-cdn-download-follows-redirect download ok'),
]


def main():
    guarded = list(weixin.glob('*.rs'))
    guarded += [native / 'channels.rs', native / 'methods/weixin_login.rs',
                weixin / 'media/tests.rs',
                root / 'src/pilotdeck-gateway/tests/weixin_channel.rs']
    guarded.extend(path for path in (fixtures / 'weixin-channel').rglob('*')
                  if path.is_file() and '__pycache__' not in path.parts)
    args = ['--check'] if sys.argv[1:] == ['--check-anchors'] else sys.argv[1:]
    return matrix_main(root, __file__, '', MUTATIONS,
                       passed_count=1, child_count=0, paths=guarded,
                       exact=True, args=args)


if __name__ == '__main__':
    raise SystemExit(main())

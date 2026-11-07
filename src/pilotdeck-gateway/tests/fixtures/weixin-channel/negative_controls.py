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
attachments = root / 'src/pilotdeck-gateway/src/channel/im_attachment.rs'
attachment_tests = (root / 'src/pilotdeck-gateway/src/channel'
                    / 'im_attachment/tests.rs')
native = root / 'src/pilotdeck-gateway/src/native_process'
REPLAY = 'channel::weixin::replay_tests::weixin_replays_the_ts_channel_oracle'
SUPERSEDED = ('channel::weixin::tests::'
               'superseded_debounce_timer_must_not_drop_the_newest_state')
ABANDONED = ('channel::weixin::tests::'
              'abandoned_drain_restores_the_claimed_state_and_waiters_retry')
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
    # #1823 appended the 31 media cases after the fifteen channel ones, so a
    # bare `pop()` drops a media case (killed by the media declaration guard)
    # instead of a channel scenario. Name which half each row drops:
    # `remove(0)` a channel case, `pop()` a media case.
    ('replay-missing-case', weixin / 'replay_tests.rs',
     '    for case in oracle_cases(&oracle) {',
     '    let mut trimmed = oracle.clone();\n'
     '    let _ = trimmed["cases"].as_array_mut().expect("#1328 mutant cases").remove(0);\n'
     '    for case in oracle_cases(&trimmed) {',
     REPLAY, '#1328 weixin oracle case count'),
    ('media-oracle-missing-case', weixin / 'replay_tests.rs',
     '    for case in oracle_cases(&oracle) {',
     '    let mut trimmed = oracle.clone();\n'
     '    let _ = trimmed["cases"].as_array_mut().expect("#1328 mutant cases").pop();\n'
     '    for case in oracle_cases(&trimmed) {',
     REPLAY, '#1823 weixin generator media declaration'),
    ('oracle-wrong-declaration', fixtures / 'weixin-channel/oracle.json',
     '  "expectedCaseCount": 20,',
     '  "expectedCaseCount": 19,',
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

    # #1822 inbound attachments. The five added channel scenarios are what
    # kills the wiring rows (c16/c20 for the fallback text and the stored
    # attachment, c18/c19 for the diagnostics paragraph, c16's
    # `source: "url"` for the branch row); the store's own order and the
    # image sniff only a mock transport can drive, so those two rows name
    # the focused cells in `channel/im_attachment/tests.rs`.
    ('weixin-attachment-fallback-text', weixin / 'mod.rs',
     'const WEIXIN_ATTACHMENT_FALLBACK_TEXT: &str = "请查看我发送的附件。";',
     'const WEIXIN_ATTACHMENT_FALLBACK_TEXT: &str = "请查看附件。";',
     REPLAY, '#1328 weixin replay oracle'),
    ('weixin-attachment-submit-dropped', weixin / 'mod.rs',
     '        if !turn.attachments.is_empty() {',
     '        if false {',
     REPLAY, '#1328 weixin replay oracle'),
    ('weixin-attachment-diagnostics-dropped', weixin / 'mod.rs',
     '        if !diagnostics.is_empty() {',
     '        if false {',
     REPLAY, '#1328 weixin replay oracle'),
    # The image item's metadata `source` branch: a `cdn_url`/`url` hit is
    # `"url"`, the `media.full_url` fallback is the other value.
    ('weixin-attachment-source-branch', weixin / 'mod.rs',
     '        metadata.push((\n'
     '            "source".into(),\n'
     '            JsValue::String(if from_url { "url" } else { "media.full_url" }.into()),\n'
     '        ));',
     '        metadata.push((\n'
     '            "source".into(),\n'
     '            JsValue::String(if from_url { "media.full_url" } else { "media.full_url" }.into()),\n'
     '        ));',
     REPLAY, '#1328 weixin replay oracle'),
    ('im-attachment-declared-size-guard-dropped', attachments,
     '        if let Some(bytes) = input.bytes\n'
     '            && bytes > self.max_bytes as f64\n'
     '        {',
     '        if let Some(bytes) = input.bytes\n'
     '            && bytes > f64::INFINITY\n'
     '        {',
     'channel::im_attachment::tests::save_from_url_guards_sizes_and_applies_the_transform',
     '#1822 declared size guard'),
    ('im-attachment-image-sniff-dropped', attachments,
     '    detect_image_mime(buffer).map(str::to_string).or(mime_type)',
     '    mime_type',
     'channel::im_attachment::tests::save_from_url_sniffs_images_over_the_response_mime',
     '#1822 image sniff wins over the declared header'),

    # #1808: the terminal comparison must reject a wrong-typed `expected`
    # instead of falling back to the old blind read.
    ('weixin-recorded-expected-wrong-type', weixin / 'replay_tests.rs',
     '        Some(expected) => Err(format!("#1328 expected must be an object, got {expected}")),',
     '        Some(expected) => Ok(expected),',
     'channel::weixin::replay_tests::recorded_expected_rejects_wrong_type',
     '#1328 #1808 rejects expected type drift'),
    # #2073: `clearTimeout` cannot be `JoinHandle::abort` (abort lands at the
    # task's next yield, after a woken timer claimed the newest state). Three
    # mutants pin the three load-bearing guards against the multi_thread repro;
    # each is killed by a different assertion.
    # 1) the in-lock identity check never rejects: the sequence stops moving,
    #    so a superseded timer believes it is still current and writes early.
    ('weixin-state-timer-superseded-ignored', weixin / 'state.rs',
     '            let seq = inner.timer_seq.entry(channel_key.clone()).or_insert(0);\n'
     '            *seq += 1;',
     '            let seq = inner.timer_seq.entry(channel_key.clone()).or_insert(0);\n'
     '            *seq += 0;',
     SUPERSEDED, '#2073 a superseded timer must not write before the debounce elapses'),
    # 2) a cancelled drain reports completion: the waiter returns a success
    #    that never wrote (this is the pre-fix shape, where the entry also
    #    stayed forever).
    ('weixin-state-drain-cancel-reported-done', weixin / 'state.rs',
     '            completed: false,',
     '            completed: true,',
     ABANDONED, '#2073 the claimed state must not be lost with the dropped drain'),
    # 3) the cancelled drain does not put back the state it claimed.
    ('weixin-state-claimed-not-restored', weixin / 'state.rs',
     '            .or_insert(state);',
     '            .or_insert(JsValue::Null);',
     ABANDONED, '#2073 the claimed state must not be lost with the dropped drain'),
    # Note: restoring `timer.abort()` on its own survives — the identity check
    # retires the woken timer before its first yield, so the abort never lands
    # (`weixin-state-timer-abort-restored` measured SURVIVED, 2026-11-07). The
    # abort removal is defence in depth; the guard is what the test pins.
    # #1808: the absent arm is a separate guard; regress it to the old blind
    # read (fall back to the case itself) and the same plain assertion dies.
    ('weixin-recorded-expected-absent', weixin / 'replay_tests.rs',
     '        None => Err("#1328 expected must be an object, got <missing>".into()),',
     '        None => Ok(value),',
     'channel::weixin::replay_tests::recorded_expected_rejects_wrong_type',
     '#1328 #1808 rejects absent expected'),

    # #1823 inbound media. The decrypt rows are killed by the focused cells in
    # `media/tests.rs` (one behaviour each): the oracle replay aborts at the
    # first mismatching case, which differs per mutation. The plaintext row
    # exists because the replay's decoy buffer is *itself* a known file and its
    # own decryption is also one, so skipping the early return adopts the decoy.
    ('media-plaintext-passthrough-dropped', weixin / 'media.rs',
     '    if is_expected(buffer) {\n        return None;\n    }',
     '    if is_expected(buffer) && buffer.is_empty() {\n        return None;\n    }',
     'channel::weixin::media::tests::'
     'plaintext_passthrough_wins_over_its_own_decryption',
     '#1823 plaintext passthrough wins over its own decryption'),
    ('media-candidates-first-key-only', weixin / 'media.rs',
     '    for key in extract_weixin_media_keys(media_owner) {\n'
     '        for deciphered in try_decrypt_weixin_media(buffer, &key) {',
     '    for key in extract_weixin_media_keys(media_owner).into_iter().take(1) {\n'
     '        for deciphered in try_decrypt_weixin_media(buffer, &key) {',
     'channel::weixin::media::tests::the_candidate_loop_reaches_the_second_key',
     '#1823 candidate loop reaches the second key'),
    ('media-key-length-always-128', weixin / 'media.rs',
     '        24 => decrypt_blocks::<Aes192>(key, &mut padded),',
     '        24 => decrypt_blocks::<Aes128>(key, &mut padded),',
     'channel::weixin::media::tests::the_key_length_selects_the_aes_variant',
     '#1823 key length selects aes-192'),
    ('media-all-miss-returns-empty', weixin / 'media.rs',
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
     '    Some(Vec::new())\n',
     'channel::weixin::media::tests::an_all_miss_falls_back_to_the_input',
     '#1823 all-miss falls back to the input'),
    ('media-pkcs7-strip-dropped', weixin / 'media.rs',
     '    let stripped = strip_pkcs7_padding(&padded).map(<[u8]>::to_vec);\n'
     '    outputs.push(stripped.unwrap_or(padded));',
     '    outputs.push(padded);',
     'channel::weixin::media::tests::decryption_strips_the_pkcs7_padding',
     '#1823 decryption strips the pkcs7 padding'),
    # The trailing-single-character guard is what keeps `chunk[1]` in range;
    # `node_base64_semantics` feeds "A" and "!".
    # `typeof [] === "object"`: the baseline's object guard admits an array
    # owner (it just has no `aeskey`/`media`), so the zero shape — not
    # `keyShape=none` — is the baseline's answer.
    ('media-array-owner-reports-none', weixin / 'media.rs',
     '    if !is_object_like(media_owner) {',
     '    if media_owner.as_object().is_none() {',
     'channel::weixin::media::tests::array_media_owner_reports_the_zero_shape_like_typeof',
     '#1823 array owner reports the zero shape'),
    ('media-base64-single-char-guard', weixin / 'media.rs',
     '        if chunk.len() < 2 {\n            break;\n        }',
     '        if chunk.len() < 2 && chunk.is_empty() {\n            break;\n        }',
     'channel::weixin::media::tests::node_base64_semantics',
     '#1823 base64 decode'),
    # The redirect policy itself now lives in the shared `fetch_redirect`
    # stack (covered by its own suites); this row guards the 50 MiB cap on the
    # *received* size, which no oracle case can record (a 50 MiB body would
    # bloat the fixture), so a dedicated loopback cell kills it.
    ('media-download-received-cap-removed', weixin / 'media.rs',
     '        let bytes = read_body(\n            response.into_body(),\n            WEIXIN_ATTACHMENT_MAX_BYTES,',
     '        let bytes = read_body(\n            response.into_body(),\n            usize::MAX,',
     'channel::weixin::media::tests::the_received_body_is_capped_at_the_limit',
     '#1823 received body is capped at the limit'),
    # Baseline scopes the 60 s `AbortController` to `fetch` itself: the timer
    # is cleared before `arrayBuffer()`, and the body is bounded only by
    # undici's 300 s `bodyTimeout`. Handing the fetch deadline to the body
    # read is exactly the regression the slow-body cell pins.
    ('media-download-body-on-fetch-deadline', weixin / 'media.rs',
     '            Some(WEIXIN_CDN_BODY_TIMEOUT),',
     '            Some(self.fetch_timeout),',
     'channel::weixin::media::tests::a_slow_body_outlives_the_fetch_deadline',
     '#1823 slow body is not bounded by the fetch deadline'),
    # The other half of the same deadline: the header phase must still abort.
    # One hour is a plain stand-in for "no deadline"; the cell's server gives
    # up after 5 s, so the mutant fails on the receipt either way.
    ('media-download-fetch-deadline-removed', weixin / 'media.rs',
     '        let response = timeout(\n            self.fetch_timeout,',
     '        let response = timeout(\n            Duration::from_secs(3600),',
     'channel::weixin::media::tests::the_header_phase_still_aborts_at_the_fetch_deadline',
     '#1823 stalled header phase aborts at the fetch deadline'),
    # `Number.isFinite(Number("Infinity"))` is false, so the baseline skips the
    # declared-size guard; keeping the infimum would reject a response the
    # baseline accepts.
    ('media-download-content-length-non-finite-guard-dropped', weixin / 'media.rs',
     '    number.is_finite().then_some(number)',
     '    Some(number)',
     'channel::weixin::media::tests::content_length_uses_js_number_finite_semantics',
     '#1823 non-finite content-length skips the guard'),
    # A body that errors mid-stream is `TypeError: terminated` (undici's
    # `arrayBuffer()` rejection), not the connection-level `fetch failed`.
    ('media-download-terminated-mapped-to-fetch-failed', weixin / 'media.rs',
     '            BodyError::Terminated => WeixinClientError {\n'
     '                name: "TypeError".into(),\n'
     '                message: "terminated".into(),\n'
     '            },',
     '            BodyError::Terminated => WeixinClientError::fetch_failed(),',
     'channel::weixin::media::tests::a_terminated_body_reports_the_fetch_type_error',
     '#1823 truncated body reports terminated'),
]


def main():
    guarded = list(weixin.glob('*.rs'))
    guarded += [native / 'channels.rs', native / 'methods/weixin_login.rs',
                weixin / 'media/tests.rs',
                root / 'src/pilotdeck-gateway/tests/weixin_channel.rs']
    guarded += [attachments, attachment_tests]
    guarded.extend(path for path in (fixtures / 'weixin-channel').rglob('*')
                  if path.is_file() and '__pycache__' not in path.parts)
    args = ['--check'] if sys.argv[1:] == ['--check-anchors'] else sys.argv[1:]
    return matrix_main(root, __file__, '', MUTATIONS,
                       passed_count=1, child_count=0, paths=guarded,
                       exact=True, args=args)


if __name__ == '__main__':
    raise SystemExit(main())

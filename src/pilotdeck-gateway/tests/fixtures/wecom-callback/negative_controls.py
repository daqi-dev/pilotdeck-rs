#!/usr/bin/env python3
"""#1336 HEAD-only wecom-callback controls; compile each selected mutant before judging it.

OUT [mutation ...], --compile-only OUT, --check (also --check-anchors).
The shared runner owns source restoration, offline cargo, nonce receipts,
patch evidence, pending registrations and the complete result summary.

The whole `channel/wecom_callback/` tree and this fixture directory are new
files of #1336, so the matrix cannot run until the patch has landed in HEAD
(`mutating_sources` refuses uncommitted product files); the script ships with
the patch and is executed by the coordinator afterwards on the landed tree.
"""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_suite import matrix_main

root = Path(__file__).resolve().parents[5]
fixtures = Path(__file__).resolve().parents[1]
source = root / 'src/pilotdeck-gateway/src/channel/wecom_callback'
tests = source / 'tests.rs'
MUTATIONS = [
    # The half-close tolerance is what lets a client that already FINed its
    # write side still receive the drained response (Node socket semantics);
    # hyper's default errors the busy connection instead. The stop-drains
    # inflight cell of the oracle reads its response only through it.
    ('half-close-disabled', source / 'http.rs',
     '.half_close(true)',
     '.half_close(false)',
     'channel_matches_real_ts', '#1336 case stop-drains-inflight matches the TS oracle'),
    # The 16-byte random prefix must be skipped before the 4-byte length.
    # First cell to notice is the GET echostr decrypt (http-get-faces), not the
    # POST dispatch cell: echostr decryption runs through the same helper.
    ('decrypt-prefix-kept', source / 'crypto.rs',
     'let content = &raw[16.min(end)..end];',
     'let content = &raw[..end];',
     'channel_matches_real_ts', '#1336 case http-get-faces matches the TS oracle'),
    # JavaScript sort is UTF-16 code-unit order; a Rust byte-order sort is
    # the exact mistake the component pin exists to catch.
    ('signature-byte-order', source / 'crypto.rs',
     'parts.sort();',
     'parts.sort_by(|a, b| String::from_utf16_lossy(a).cmp(&String::from_utf16_lossy(b)));',
     'signature_sort_uses_utf16_code_units',
     '#1336 signature sort follows UTF-16 code-unit order'),
    # errcode 40014 must clear the cached token like the TS channel, or the
    # token-expiry-refresh cell loses its second gettoken.
    ('token-not-cleared', source / 'mod.rs',
     'if value == 40014.0 || value == 42001.0) {',
     'if value == 42001.0) {',
     'channel_matches_real_ts', '#1336 case token-expiry-refresh matches the TS oracle'),
    # A bare `/new` is a command too; dropping the equality arm sends it to
    # the general session instead of rotating the mapper.
    ('bare-new-dropped', source / 'mapper.rs',
     'if trimmed == "/new" || trimmed.starts_with("/new ") {',
     'if trimmed.starts_with("/new ") {',
     'channel_matches_real_ts', '#1336 case mapper-commands matches the TS oracle'),
    # The outbound body truncates at exactly 2048 UTF-16 units, not bytes.
    # Mutating the limit itself (rather than taking N+1) keeps the emitted body
    # valid JSON, so the pin's own assertion fires instead of the parse helper.
    ('truncate-past-limit', source / 'mod.rs',
     'const MAX_MESSAGE_LENGTH: usize = 2048;',
     'const MAX_MESSAGE_LENGTH: usize = 2046;',
     'message_body_truncates_at_2048_utf16_units',
     '#1336 3000 UTF-16 units truncate to 2048 (1024 surrogate pairs)'),
    # Dropping a case from the parsed oracle must trip the count declaration,
    # not silently pass with fewer cells.
    ('oracle-missing-case', tests,
     '    let oracle = oracle();\n    let qyapi = Qyapi::new();\n',
     '    let mut oracle = oracle();\n'
     '    let _ = oracle["cases"].as_array_mut().expect("#1336 mutant cases").pop();\n'
     '    let qyapi = Qyapi::new();\n',
     'channel_matches_real_ts', '#1336 oracle case count matches its declaration'),
    # An empty cases array must be rejected, never skipped.
    ('oracle-empty-cases', fixtures / 'wecom-callback/oracle.json',
     '  "cases": [\n',
     '  "cases": [],\n  "disabledCases": [\n',
     'channel_matches_real_ts', '#1336 oracle cases non-empty'),
    # Dropping a connection's timers must not let a half-sent request pin
    # `stop()`: hyper disables `header_read_timeout` without a timer, so the
    # fixed half of the A/B has to catch the regression on its own.
    ('header-timeout-timer-removed', source / 'http.rs',
     '                            .timer(TokioTimer::new())\n'
     '                            .header_read_timeout(timeouts.headers)\n',
     '                            .half_close(true)\n',
     'stop_drains_a_half_sent_header_once_the_header_timeout_fires',
     '#1336 headersTimeout lets stop() retire a half-sent request'),
    # Node answers a request whose body never arrives with 408, not 200.
    ('request-timeout-status', source / 'http.rs',
     '        .status(408)',
     '        .status(200)',
     'request_timeout_answers_408_and_releases_stop',
     '#1336 requestTimeout answers 408 Request Timeout'),
    # The 408 must close the connection like Node's `requestTimeout`; the
    # ledger entry (A8.3-WECOM-CALLBACK-408-001) names this mutation as its
    # machine criterion, so it has to exist here.
    ('wecom-408-without-close', source / 'http.rs',
     '        .header(header::CONNECTION, "close")\n',
     '',
     'request_timeout_answers_408_and_releases_stop',
     '#1336 the 408 closes the connection like Node\'s requestTimeout'),
    # The 408 is a zero-length response: a body would leave the client reading
    # past the pinned header terminator (A8.3-WECOM-CALLBACK-408-001). The
    # mutant body is `\r\n\r\n` on purpose — a plain `x` is caught even by a
    # `raw.ends_with("\r\n\r\n")` pin, while this one is only caught when the
    # pin splits at the *first* terminator and requires nothing after it
    # (#1336 r5 Codex P1).
    ('wecom-408-with-body', source / 'http.rs',
     '.body(super::full(Bytes::new()))',
     '.body(super::full(Bytes::from_static(b"\\r\\n\\r\\n")))',
     'request_timeout_answers_408_and_releases_stop',
     '#1336 the 408 has no body'),
    # The active-chat claim only holds if the winner inserts the chat while it
    # still owns the state lock; without the insert every concurrent POST
    # dispatches its own turn (#1336 r2 concurrency P1).
    ('active-chat-not-inserted', source / 'mod.rs',
     'state.active_chats.insert(chat_id.clone());',
     '/* claim dropped by the mutant */',
     'concurrent_posts_for_one_chat_dispatch_once',
     '#1336 concurrent posts for one chat dispatch exactly one turn'),
    # The recorded case count is 22; the replay holds the declaration to it.
    # Dropping a case *and* lowering the declaration together leaves the
    # non-empty and declaration checks green; only the generator's literal 22
    # catches the coverage loss.
    ('oracle-shrunk-list', tests,
     '    let oracle = oracle();\n    let qyapi = Qyapi::new();\n',
     '    let mut oracle = oracle();\n'
     '    let _ = oracle["cases"].as_array_mut().expect("#1336 mutant cases").pop();\n'
     '    oracle["expectedCaseCount"] = json!(21);\n'
     '    let qyapi = Qyapi::new();\n',
     'channel_matches_real_ts', "#1336 oracle holds the generator's 22 cases"),
    ('oracle-wrong-declaration', fixtures / 'wecom-callback/oracle.json',
     '"expectedCaseCount": 22,',
     '"expectedCaseCount": 21,',
     'channel_matches_real_ts', '#1336 oracle case count matches its declaration'),
    # #1941: the POST body cap. With the limit disabled the oversize cell's
    # read has to sit out the 64 MiB it declared but did not send, so it hits
    # its own deadline instead of the cap and turns red.
    ('wecom-body-cap-disabled', source / 'mod.rs',
     'const MAX_BODY_BYTES: f64 = super::webhook::DEFAULT_MAX_BODY_BYTES;',
     'const MAX_BODY_BYTES: f64 = f64::INFINITY;',
     'oversize_post_closes_at_the_cap_with_zero_bytes',
     '#1941 an oversize POST ends at the cap instead of waiting for the '
     'declared body'),
    # Answering instead of closing: A8.3-WECOM-CALLBACK-BODY-LIMIT-001 pins
    # the ending as the webhook lane's zero-byte close, never a 413.
    ('wecom-oversize-answers-413', source / 'mod.rs',
     'Err(RouteError::PayloadTooLarge) => Err(io::Error::other("payload too large")),',
     'Err(RouteError::PayloadTooLarge) => Ok(response(413, None, b"payload too large")),',
     'oversize_post_closes_at_the_cap_with_zero_bytes',
     '#1941 an oversize POST gets no response bytes (zero-byte close, not 413)'),
]


def main():
    guarded = list(source.glob('*.rs'))
    guarded.extend(path for path in (fixtures / 'wecom-callback').rglob('*') if path.is_file())
    args = ['--check'] if sys.argv[1:] == ['--check-anchors'] else sys.argv[1:]
    return matrix_main(root, __file__, 'channel::wecom_callback::tests::', MUTATIONS,
                       passed_count=1, child_count=0, paths=guarded,
                       exact=True, args=args)


if __name__ == '__main__':
    raise SystemExit(main())

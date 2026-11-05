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
    # The recorded case count is 22; the replay holds the declaration to it.
    ('oracle-wrong-declaration', fixtures / 'wecom-callback/oracle.json',
     '"expectedCaseCount": 22,',
     '"expectedCaseCount": 21,',
     'channel_matches_real_ts', '#1336 oracle case count matches its declaration'),
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

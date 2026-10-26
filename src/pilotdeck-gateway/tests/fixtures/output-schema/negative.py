#!/usr/bin/env python3
"""#829/#834 negative controls: break the JS flag emulation, require red.

Every mutant changes one production behavior in output_schema.rs (the ASCII
fold mask, the `(?i)` the port now adds where ajv-formats compiles with
`/i`, the #834 `url` `/iu` fold, or the `byte` `/gm` line split). While a
mutant is live, `formats_follow_ajv_formats` and — for the oracle mutants —
`replays_the_ajv_oracle` must FAIL on the pinned rows; after
`mutating_sources` restores the bytes, the same command must pass (the caller
runs the unmutated suite separately). Compile failures and timeouts are
INVALID evidence. Run exclusively; cargo uses the caller's CARGO_TARGET_DIR.

Usage:
  python3 negative.py <output-dir> [mutant ...]   # default: all mutants
"""
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

fixture = Path(__file__).resolve().parent
rust = fixture.parents[3]
repo = rust.parent
output = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else fixture / '.negative'
output.mkdir(parents=True, exist_ok=True)
source = rust / 'pilotdeck-gateway/src/runtime/mcp/output_schema.rs'
slots = repo / 'scripts/cargo-slots.sh'

if not os.environ.get('CARGO_TARGET_DIR'):
    sys.exit('set CARGO_TARGET_DIR (the caller owns the build directory)')
if not os.environ.get('PDRS_BASELINE'):
    sys.exit('set PDRS_BASELINE (the Ajv oracle replay needs it)')

# (name, old, new, oracle replay too?) — old/new are exact byte strings of
# output_schema.rs; every old must occur exactly once so a drifted anchor
# refuses to run instead of silently mutating nothing.
MUTANTS = [
    ('email-mask-skipped',
     b'"email" => regexes.email.is_match(&ascii_fold_haystack(text)),',
     b'"email" => regexes.email.is_match(text),', True),
    ('fragment-mask-skipped',
     b'"json-pointer-uri-fragment" => regexes\n            .json_pointer_uri_fragment\n            .is_match(&ascii_fold_haystack(text)),',
     b'"json-pointer-uri-fragment" => regexes\n            .json_pointer_uri_fragment\n            .is_match(text),', False),
    ('uri-mask-skipped',
     b'regexes.uri.is_match(&ascii_fold_haystack(text))',
     b'regexes.uri.is_match(text)', False),
    ('uri-reference-mask-skipped',
     b'regexes.uri_reference.is_match(&ascii_fold_haystack(text))',
     b'regexes.uri_reference.is_match(text)', False),
    ('uri-template-mask-skipped',
     b'regexes.uri_template.is_match(&ascii_fold_haystack(text))',
     b'regexes.uri_template.is_match(text)', False),
    ('uri-i-dropped',
     b'r#"(?i)^(?:[a-z][a-z0-9+\\-.]*:)(?:\\/?\\/(?:(?:[a-z0-9\\-._~!$&\'()*+,;=:]|%[0-9a-f]{2})*@)?(?:\\[(?:(?:(?:(?:[0-9a-f]{1,4}:){6}',
     b'r#"^(?:[a-z][a-z0-9+\\-.]*:)(?:\\/?\\/(?:(?:[a-z0-9\\-._~!$&\'()*+,;=:]|%[0-9a-f]{2})*@)?(?:\\[(?:(?:(?:(?:[0-9a-f]{1,4}:){6}', False),
    ('uri-reference-i-dropped',
     b'r#"(?i)^(?:[a-z][a-z0-9+\\-.]*:)?(?:\\/?\\/(?:(?:[a-z0-9\\-._~!$&\'()*+,;=:]|%[0-9a-f]{2})*@)?(?:\\[(?:(?:(?:(?:[0-9a-f]{1,4}:){6}',
     b'r#"^(?:[a-z][a-z0-9+\\-.]*:)?(?:\\/?\\/(?:(?:[a-z0-9\\-._~!$&\'()*+,;=:]|%[0-9a-f]{2})*@)?(?:\\[(?:(?:(?:(?:[0-9a-f]{1,4}:){6}', False),
    ('uri-template-i-dropped',
     b'r#"(?i)^(?:(?:[^\\x00-\\x20"\'<>%\\\\^`{|}]|%[0-9a-f]{2})|\\{[+#./;?&=,!@|]?(?:[a-z0-9_]|%[0-9a-f]{2})+',
     b'r#"^(?:(?:[^\\x00-\\x20"\'<>%\\\\^`{|}]|%[0-9a-f]{2})|\\{[+#./;?&=,!@|]?(?:[a-z0-9_]|%[0-9a-f]{2})+', False),
    # fix-r1: ipv6's own (?i) had no mutant, and the review asked for IPvFuture
    # coverage. Dropping (?i) must flip the ::ABCD rows (unit + oracle).
    ('ipv6-i-dropped',
     b'r#"(?i)^((([0-9a-f]{1,4}:){7}([0-9a-f]{1,4}|:))',
     b'r#"^((([0-9a-f]{1,4}:){7}([0-9a-f]{1,4}|:))', True),
    # Simulates "fixing" ipv6 by grafting the uri regex's IPvFuture branch on:
    # the SDK has no such branch, so the v1.s / v1.ſ rows must flip red. The
    # branch goes inside the outermost group (after the `|:))` that closes the
    # `::` alternative's branch group) so both anchors still apply to it.
    ('ipv6-ipvfuture-branch-added',
     b'|:)))$"#,',
     b'|:))|[Vv][0-9a-f]+\\.[a-z0-9\\-._~!$&\'()*+,;=:]+)$"#,', True),
    # Masking must mask, not fold: replacing U+017F with a real `s` makes the
    # port accept what the SDK rejects (and would also fold U+212A onto `s`).
    ('mask-folds-not-masks',
     b'text.replace([\'\\u{017F}\', \'\\u{212A}\'], "\\u{FFFD}")',
     b'text.replace([\'\\u{017F}\', \'\\u{212A}\'], "s")', True),
    # The mask's own guard: folding İ onto `i` must flip the İ golden rows.
    ('mask-folds-i-with-dot',
     b'if text.contains([\'\\u{017F}\', \'\\u{212A}\']) {\n'
     b'        std::borrow::Cow::Owned(text.replace([\'\\u{017F}\', \'\\u{212A}\'], "\\u{FFFD}"))',
     b'if text.contains([\'\\u{017F}\', \'\\u{212A}\', \'\\u{0130}\']) {\n'
     b'        std::borrow::Cow::Owned(text.replace(\'\\u{017F}\', "s").replace(\'\\u{0130}\', "i").replace(\'\\u{212A}\', "\\u{FFFD}"))',
     False),
    # #834: `url` is /iu, so ſ must fold onto the scheme's `s`; dropping the
    # fold restores the false reject of HTTPſ://.
    ('url-long-s-not-folded',
     b'    let lower = if lower.contains(\'\\u{17f}\') {\n'
     b'        std::borrow::Cow::Owned(lower.replace(\'\\u{17f}\', "s"))\n'
     b'    } else {\n'
     b'        std::borrow::Cow::Borrowed(lower.as_str())\n'
     b'    };\n'
     b'    let rest = lower',
     b'    let lower = std::borrow::Cow::Borrowed(lower.as_str());\n'
     b'    let rest = lower',
     True),
    # #834: `byte` is /gm — \\r and U+2028/U+2029 are line terminators too;
    # recognizing only \\n is exactly the bug #834 fixes.
    ('byte-newline-only',
     b'    text.split([\'\\n\', \'\\r\', \'\\u{2028}\', \'\\u{2029}\'])\n'
     b'        .any(|line| formats().byte.is_match(line))',
     b'    text.split(\'\\n\')\n'
     b'        .any(|line| formats().byte.is_match(line))',
     True),
    # #834: `/gm` test semantics is "any full line matches", not "all";
    # aGVsbG8=\\r!! is valid on its first line.
    ('byte-all-lines-required',
     b'        .any(|line| formats().byte.is_match(line))',
     b'        .all(|line| formats().byte.is_match(line))',
     True),
]
selected = sys.argv[2:] or [name for name, _old, _new, _oracle in MUTANTS]

env = dict(os.environ, CARGO_NET_OFFLINE='true')
test = 'runtime::mcp::output_schema::tests::formats_follow_ajv_formats'
replay = 'runtime::mcp::output_schema::tests::replays_the_ajv_oracle'
baseline = os.environ.get('PDRS_NEGATIVE_BASELINE', 'HEAD')
failures = []
with mutating_sources(repo, [source], baseline=baseline) as snapshots:
    original = snapshots[source]
    for name, old, new, oracle in MUTANTS:
        if name not in selected:
            continue
        if original.count(old) != 1:
            failures.append(f'{name}: anchor occurs {original.count(old)} times (expected 1)')
            continue
        log = output / f'{name}.log'
        source.write_bytes(original.replace(old, new))
        wait_for_slot(slots)
        result = run_owned(
            ['cargo', 'test', '--manifest-path', str(rust / 'Cargo.toml'),
             '-p', 'pilotdeck-gateway', '--lib', '--', test],
            env=env, cwd=repo, timeout=1200)
        log.write_bytes(result.stdout.encode() + result.stderr.encode())
        red = result.returncode != 0
        detail = 'FAIL(good)' if red else 'PASS(bad: mutant survived)'
        print(f'{name}: unit {detail}')
        if not red:
            failures.append(f'{name}: unit test still green under the mutant')
            continue
        if oracle:
            wait_for_slot(slots)
            result = run_owned(
                ['cargo', 'test', '--manifest-path', str(rust / 'Cargo.toml'),
                 '-p', 'pilotdeck-gateway', '--lib', '--', replay],
                env=env, cwd=repo, timeout=1200)
            with log.open('ab') as sink:
                sink.write(b'\n--- oracle replay ---\n')
                sink.write(result.stdout.encode() + result.stderr.encode())
            red = result.returncode != 0
            print(f'{name}: oracle {"FAIL(good)" if red else "PASS(bad: mutant survived)"}')
            if not red:
                failures.append(f'{name}: oracle replay still green under the mutant')

if failures:
    for failure in failures:
        print(f'NEGATIVE CONTROL FAILED: {failure}', file=sys.stderr)
    sys.exit(1)
print(f'all {len(selected)} mutants rejected, sources restored')

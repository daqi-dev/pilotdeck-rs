#!/usr/bin/env python3
"""#829/#834/#848 negative controls: break the JS emulation, require red.

Every mutant changes one production behavior in output_schema.rs (the ASCII
fold mask, the `(?i)` the port now adds where ajv-formats compiles with
`/i`, the #834 `url` `/iu` fold, the `byte` `/gm` line split, the #848
JS `\\s` replacement for Rust's `char::is_whitespace`, or the #848 fix-r1
`url` userinfo/`/` boundary and the #848 fix-r2 `url` label hyphen
rule). While a mutant is
live, `formats_follow_ajv_formats` and — for the oracle mutants —
`replays_the_ajv_oracle` must FAIL on the pinned rows; after
`mutating_sources` restores the bytes, the same command must pass (the caller
runs the unmutated suite separately). Compile failures and timeouts are
INVALID evidence. Run exclusively; cargo uses the caller's CARGO_TARGET_DIR.

Usage:
  python3 negative.py <output-dir> [mutant ...]   # default: all mutants
  python3 negative.py --check-anchors             # static only, no mutation
"""
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

fixture = Path(__file__).resolve().parent
rust = fixture.parents[3]
repo = rust.parent
source = rust / 'pilotdeck-gateway/src/runtime/mcp/output_schema.rs'
slots = repo / 'scripts/cargo-slots.sh'
check_anchors = '--check-anchors' in sys.argv
if check_anchors:
    output = None
else:
    output = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else fixture / '.negative'
    output.mkdir(parents=True, exist_ok=True)

if not check_anchors and not os.environ.get('CARGO_TARGET_DIR'):
    sys.exit('set CARGO_TARGET_DIR (the caller owns the build directory)')
if not check_anchors and not os.environ.get('PDRS_BASELINE'):
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
    # fix-r1: the reviewer's proposed "fix" — collapse \r\n into a single
    # terminator before splitting — diverges from V8: the empty line between
    # \r and \n is a full line of its own, so the \r\n unit rows and the
    # byte/crlf-any-line / byte/crlf-bad-lines oracle cases must flip red.
    ('byte-crlf-one-terminator',
     b'    text.split([\'\\n\', \'\\r\', \'\\u{2028}\', \'\\u{2029}\'])\n'
     b'        .any(|line| formats().byte.is_match(line))',
     b'    text.replace("\\r\\n", "\\r")\n'
     b'        .split([\'\\n\', \'\\r\', \'\\u{2028}\', \'\\u{2029}\'])\n'
     b'        .any(|line| formats().byte.is_match(line))',
     True),
    # #848: the helper calls must use the repo's canonical JS `\s` predicate
    # (`metadata::js_space`), not Rust's `char::is_whitespace`. Each call site
    # gets its own mutant so a single reverted line cannot hide behind the
    # others. `url` path is `[^\s]*`; `last_space` is only read by the path
    # test, so reverting it is the path control.
    ('url-path-space-reverted',
     b'    let last_space = rest\n'
     b'        .char_indices()\n'
     b'        .rev()\n'
     b'        .find(|(_, c)| js_space(*c))\n'
     b'        .map(|(index, _)| index);',
     b'    let last_space = rest\n'
     b'        .char_indices()\n'
     b'        .rev()\n'
     b'        .find(|(_, c)| c.is_whitespace())\n'
     b'        .map(|(index, _)| index);',
     True),
    # #848: userinfo is `\S+`; `first_space` is only read by the userinfo test.
    # Reverting only this line lets BOM through while the path and date-time
    # helpers stay correct.
    ('url-userinfo-space-reverted',
     b'    let first_space = rest\n'
     b'        .char_indices()\n'
     b'        .find(|(_, c)| js_space(*c))\n'
     b'        .map(|(index, _)| index);',
     b'    let first_space = rest\n'
     b'        .char_indices()\n'
     b'        .find(|(_, c)| c.is_whitespace())\n'
     b'        .map(|(index, _)| index);',
     False),
    # #848: the host classes are `[a-z0-9\u{00a1}-\u{ffff}]`, so JS whitespace
    # such as BOM/U+2000 must stay valid in the host. Stopping the host run at
    # any JS space reproduces the removed blanket authority rejection.
    ('url-host-space-filtered',
     b'            if !(is_uri_host_char(c) || c == \'-\' || c == \'.\') {\n'
     b'                break;\n'
     b'            }',
     b'            if !(is_uri_host_char(c) || c == \'-\' || c == \'.\') || js_space(c) {\n'
     b'                break;\n'
     b'            }',
     False),
    # #848 fix-r1: the authority walk now owns `(?::\d{2,5})?`; dropping the
    # upper bound accepts a 6-digit port that the SDK rejects. The reject-side
    # unit rows above are this mutant's control.
    ('url-port-too-long',
     b'                    if !(2..=5).contains(&(port_end - digits)) {',
     b'                    if port_end - digits < 2 {',
     False),
    # #848 fix-r1 (review P1): the regression is treating the FIRST `/` as the
    # authority boundary. The regex's userinfo `\S+` may contain `/`, so an
    # `@` after the first `/` can still start the host. Only honouring `@`
    # before the first `/` restores HEAD's wrong split: the review
    # counterexample then reads the BOM as a path character and must go red.
    ('url-first-slash-boundary',
     b'    accept(0) || rest.match_indices(\'@\').any(|(index, _)| accept(index + 1))',
     b'    let first_slash = rest.find(\'/\').unwrap_or(rest.len());\n'
     b'    accept(0)\n'
     b'        || rest\n'
     b'            .match_indices(\'@\')\n'
     b'            .filter(|(index, _)| *index < first_slash)\n'
     b'            .any(|(index, _)| accept(index + 1))',
     True),
    # #848 fix-r2 (review P1): an ajv-formats label is
    # `(?:[host]+-)*[host]+`, so `--` is impossible. Dropping only the new
    # consecutive-hyphen guard restores the false accept of
    # `http://user/x@exam--ple.com/`; the double/triple-hyphen unit rows and
    # the url/host-*hyphen* / url/userinfo-slash-host-*hyphen* oracle cases
    # must turn red, while the single-hyphen rows stay green.
    ('url-host-double-hyphen-allowed',
     b'            || label.contains("--")\n',
     b'',
     True),
    # #848: date-time has its own `/t|\s/i` split. Reverting only it keeps the
    # url helpers fixed, so the date-time rows are proven to be its own
    # control rather than riding on the url rows.
    ('date-time-space-reverted',
     b'        .split(|c: char| c == \'t\' || c == \'T\' || js_space(c))',
     b'        .split(|c: char| c == \'t\' || c == \'T\' || c.is_whitespace())',
     True),
]
if check_anchors:
    original = source.read_bytes()
    bad = [
        f'{name}: anchor occurs {original.count(old)} times (expected 1)'
        for name, old, _new, _oracle in MUTANTS
        if original.count(old) != 1
    ]
    if bad:
        print('\n'.join(bad), file=sys.stderr)
        sys.exit(1)
    print(f'all {len(MUTANTS)} anchors occur exactly once')
    sys.exit(0)
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

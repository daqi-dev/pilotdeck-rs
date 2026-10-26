#!/usr/bin/env python3
"""Reversible product mutations for edgeclaw-llm-envelope.

Every case must compile and fail a runtime oracle assertion. Every anchor is
checked against the baseline bytes before the unmutated suite runs: a stale
anchor is a harness defect, not a surviving mutant, so it prints
``STALE ANCHOR`` and exits 2 (same rule as #772 fix-r2). A surviving case
does not stop the run: all selected cases are executed, the script prints a
KILLED / SURVIVED summary and exits 1 when anything survived or is invalid.
The original bytes are restored after every case and again in ``finally``;
oracle.json is never touched.

Run all cases (the fixture is committed, so ``mutating_sources`` can read it):

    CARGO_TARGET_DIR=/abs/target python3 negative.py /tmp/edgeclaw-envelope-negative

Run the decode-crlf sample first, or list the names:

    CARGO_TARGET_DIR=/abs/target python3 negative.py /tmp/edgeclaw-envelope-negative decode-crlf
    python3 negative.py --list

The ``# oracle`` comments name the inputs whose expected value flips; the
RESULT for fix-r1 lists the exact ``cases[i]`` index of every witness.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/llm/envelope.rs'

cases = [
    # Drop the brace-bounded fast path: a multi-object response returns only the first object.
    # oracle: object #13 `{"a":1}{"b":2}`, #17/#19 trailing `}` after prose.
    ('object-shortcut', source,
     '    if trimmed[0] == OPEN_BRACE && trimmed[trimmed.len() - 1] == CLOSE_BRACE {',
     '    if trimmed[0] == OPEN_BRACE && trimmed[trimmed.len() - 1] == CLOSE_BRACE && false {',
     'json_object_matches_ts'),
    # Stop honouring backslash escapes while scanning; an escaped quote closes the string early.
    # oracle: object #10 escaped quote, #24 nested escaped quote.
    ('object-escape', source,
     '            } else if unit == BACKSLASH {\n                escaped = true;',
     '            } else if unit == BACKSLASH && false {\n                escaped = true;',
     'json_object_matches_ts'),
    # Envelope end-exclusive off-by-one drops the final `}`.
    # oracle: envelope #35 `json {"a":1} end` (and every success shape).
    ('envelope-slice', source,
     'Ok(JsString::from_utf16(trimmed[start..=end].to_vec()))',
     'Ok(JsString::from_utf16(trimmed[start..end].to_vec()))',
     'json_envelope_matches_ts'),
    # Skip the final double-backslash pass, so doubled backslashes survive.
    # oracle: string #103 `{"markdown":"a\\b"}` (bound test).
    ('decode-final-unescape', source,
     '        (b"\\\\\\\\".as_slice(), b"\\\\".as_slice()),',
     '        (b"\\\\\\\\".as_slice(), b"\\\\\\\\".as_slice()),',
     'loose_string_matches_ts'),
    # Drop the CR-LF pass; CR then LF decode to a CR LF pair instead of LF.
    # oracle: string #84-#86 and #104-#105 literal `\r\n` (bound test).
    ('decode-crlf', source,
     '        (b"\\\\r\\\\n".as_slice(), b"\\n".as_slice()),',
     '        (b"\\\\r\\\\n".as_slice(), b"\\\\r\\\\n".as_slice()),',
     'loose_string_matches_ts'),
    # Lazy -> greedy capture: a duplicated next key extends the string group.
    # oracle: string #74 (name then description) and #83 (duplicate name).
    ('string-lazy-capture', source,
     '    (start..units.len()).find(|&end| units[end] == QUOTE && comma_key_at(units, end + 1, next_keys))',
     '    (start..units.len()).rev().find(|&end| units[end] == QUOTE && comma_key_at(units, end + 1, next_keys))',
     'loose_string_matches_ts'),
    # Treat every negative slice bound as zero: `slice(0, -2)` loses its tail.
    # oracle: truncate #171 (bound test).
    ('slice-negative', source,
     '''    let end = end.trunc();
    if end < 0.0 {
        let shifted = len as f64 + end;
        if shifted <= 0.0 { 0 } else { shifted as usize }
    } else {
        end as usize
    }''',
     '''    let end = end.trunc();
    if end < 0.0 {
        0
    } else {
        end as usize
    }''',
     'truncation_matches_ts'),
    # Push duplicates: keep every trimmed item instead of the first one.
    # oracle: uniqueStrings #157/#158 duplicate/whitespace-deduped items.
    ('unique-dedupe', source,
     '        if seen.insert(value.clone()) {\n            out.push(value);',
     '        if seen.insert(value.clone()) || true {\n            out.push(value);',
     'string_arrays_match_ts'),
    # Route matching is case-sensitive; accept `USER` and the oracle flips.
    # oracle: route #209 `USER`.
    ('route-case', source,
     'JsValue::String(text) if text.equals_str("user") => MemoryRoute::User,',
     'JsValue::String(text) if text.equals_str("user") || text.equals_str("USER") => MemoryRoute::User,',
     'route_and_boolean_match_ts'),
    # Drop `toLowerCase`: `"TRUE"` no longer parses as true.
    # oracle: boolean #222 `TRUE` and #234 `TrUe`.
    ('boolean-case', source,
     '            let normalized = lower(&trimmed, &LanguageIdentifier::UNKNOWN, false);',
     '            let normalized = trimmed;',
     'route_and_boolean_match_ts'),
    # The loose payload requires all three strings; dropping markdown accepts
    # payloads that TS rejects as null.
    # oracle: payload #109 (name + description, no markdown).
    ('payload-required', source,
     'if payload.name.is_some() && payload.description.is_some() && payload.markdown.is_some() {',
     'if payload.name.is_some() && payload.description.is_some() {',
     'loose_payload_matches_ts'),
    # Swallow the envelope error into `null`: TS throws for empty/no-brace text.
    # oracle: payload #118 `""` and #120/#121 no-envelope.
    ('payload-error', source,
     '    let envelope = extract_loose_json_envelope(raw)?;',
     '    let Ok(envelope) = extract_loose_json_envelope(raw) else { return Ok(None) };',
     'loose_payload_matches_ts'),
    # The greedy group's closing quote must have only JS whitespace before the
    # final `}`; without that check `{"markdown":"x"} }` parses.
    # oracle: string #102 `{"markdown":"x"} }`.
    ('string-greedy-gap', source,
     '.any(|&unit| !js_is_space_unit(unit))',
     '.any(|&unit| unit == u16::MAX)',
     'loose_string_matches_ts'),
    # Do not consume the trailing JS whitespace before `$`: a final newline
    # turns a valid markdown capture into no capture.
    # oracle: string #96 final `\n`, #101 trailing spaces.
    ('string-greedy-trailing', source,
     '    while end > 0 && js_is_space_unit(units[end - 1]) {',
     '    while end > 0 && units[end - 1] == u16::MAX {',
     'loose_string_matches_ts'),
    # truncateForPrompt must normalize before truncating; whitespace survives.
    # oracle: truncateForPrompt #200 `  a   b  ` and #204 NBSP.
    ('prompt-normalize', source,
     '    truncate(&normalize_whitespace(value), max_length)',
     '    truncate(value, max_length)',
     'truncation_matches_ts'),
    # Non-unicode `/i` folds ASCII; without folding `"SKIP"` misses.
    # oracle: loose-bool #47 `"SKIP":TrUe` (bound test).
    ('boolean-ascii-fold', source,
     '        (true, true) => ascii_upper(left) == ascii_upper(right),',
     '        (true, true) => left == right,',
     'loose_boolean_matches_ts'),
    # Non-ASCII pairs fold through their upper-case mapping; `"é"` != `"É"`.
    # oracle: loose-bool #62 `"é"`/`É` and #65 `"ς"`/`σ` (bound test).
    ('boolean-nonascii-fold', source,
     '        (false, false) => simple_upper(left) == simple_upper(right),',
     '        (false, false) => false,',
     'loose_boolean_matches_ts'),
    # -- P2 r1: branches that the first 17 mutants did not reach ------------
    # Drop the single-string branch of normalizeStringArray.
    # oracle: arrays #129 `"  a  "`, #135 `" a b "`, #152 NBSP string.
    ('array-single-string', source,
     '    match items {\n        JsValue::String(text) => {',
     '    match items {\n        JsValue::String(text) if false => {',
     'string_arrays_match_ts'),
    # Keep blank single strings instead of dropping them.
    # oracle: arrays #130 `""` and #131 `"   "`.
    ('array-single-blank', source,
     '            let trimmed = js_trim_units(text.units());\n            if !trimmed.is_empty() {',
     '            let trimmed = js_trim_units(text.units());\n            if true {',
     'string_arrays_match_ts'),
    # Coerce non-string array items to one value instead of filtering them out.
    # oracle: arrays #139 mixed `[1,"x",null,"  ",true,"y"]`, #148 `[null]`.
    ('array-nonstring-filter', source,
     '                let JsValue::String(text) = item else {\n                    continue;\n                };',
     '''                let fallback = JsString::from("x");
                let text = match item {
                    JsValue::String(text) => text,
                    _ => &fallback,
                };''',
     'string_arrays_match_ts'),
    # Keep blank array items instead of dropping them.
    # oracle: arrays #139 mixed list (the `"  "` item).
    ('array-blank-filter', source,
     '                let trimmed = js_trim_units(text.units());\n                if !trimmed.is_empty() {',
     '                let trimmed = js_trim_units(text.units());\n                if true {',
     'string_arrays_match_ts'),
    # A non-array/non-string input yields one blank item instead of [].
    # oracle: arrays #149 `{}`, #150 `true`, #151 `0`.
    ('array-non-array', source,
     '        _ => return Vec::new(),',
     '        _ => return vec![JsString::from("")],',
     'string_arrays_match_ts'),
    # `match?.[1]` treats an empty capture as no value; return `Some("")`.
    # oracle: string #72 and #94 empty captures.
    ('string-empty-capture', source,
     '        if group_end == group_start {\n            return None;\n        }',
     '        if group_end == group_start && false {\n            return None;\n        }',
     'loose_string_matches_ts'),
    # Forget the `false` alternative: only `true` is recognized.
    # oracle: loose-bool #46 `FALSE`, #56 first of two duplicated keys.
    ('boolean-false-literal', source,
     '        for word in ["true", "false"] {',
     '        for word in ["true"] {',
     'loose_boolean_matches_ts'),
    # Drop `/i` folding at the key_at call site (the value word still folds).
    # oracle: loose-bool #47 uppercase key, #62 `é`/`É` (bound test).
    ('key-case-fold', source,
     '        .all(|(index, &unit)| unit_equals_ignoring_case(units[at + 1 + index], unit))',
     '        .all(|(index, &unit)| units[at + 1 + index] == unit)',
     'loose_boolean_matches_ts'),
    # Accept any next key instead of filtering by the next_keys set.
    # oracle: string #73 `"unknown"`, #77 `"nameless"`.
    ('comma-next-key-filter', source,
     '''    next_keys.iter().any(|key| {
        key_at(units, index, key.units())
            && units.get(skip_space(units, index + key.len_utf16() + 2)) == Some(&COLON)
    })''',
     '    next_keys.iter().any(|_| true)',
     'loose_string_matches_ts'),
]

names = [case[0] for case in cases]
assert len(names) == len(set(names)), 'duplicate control name'
all_cases = list(cases)
if sys.argv[1:] == ['--list']:
    print('\n'.join(names))
    raise SystemExit(0)
assert len(sys.argv) >= 2, 'usage: negative.py OUT_DIR [NAME ...] | negative.py --list'
selected = sys.argv[2:]
unknown = set(selected) - set(names)
assert not unknown, f'unknown controls: {sorted(unknown)}'
if selected:
    cases = [case for case in cases if case[0] in selected]
assert cases, 'no controls selected'

out = Path(sys.argv[1]).resolve()
target = os.environ.get('CARGO_TARGET_DIR')
assert target, 'negative.py requires CARGO_TARGET_DIR from the caller'
out.mkdir(parents=True, exist_ok=True)

env = dict(os.environ)
env['CARGO_TARGET_DIR'] = target
command = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
           '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_llm_envelope']
results = []


def classify(log, returncode):
    if returncode is None:
        return 'INVALID'
    if 'could not compile' in log:
        return 'INVALID'
    if returncode != 0 and 'test result: FAILED.' in log and 'running 1 test' in log:
        return 'KILLED'
    if returncode == 0 and 'test result: ok.' in log:
        return 'SURVIVED'
    return 'INVALID'


def write_summary(baseline):
    (out / 'summary.json').write_text(json.dumps(
        {'baseline': baseline, 'cases': results}, indent=2) + '\n')


with mutating_sources(root, [source]) as originals:
    original = originals[source].decode()
    # An anchor that this list did not follow is a harness defect, not a
    # classified cell: INVALID/exit 1 still blames the wrong thing and lets a
    # broken control hide behind an environment or compile fault. Refuse
    # before compiling.
    stale = []
    for name, _path, before, _after, _test in all_cases:
        count = original.count(before)
        if count != 1:
            stale.append((name, source.relative_to(root).as_posix(), count))
    if stale:
        report = '\n'.join(
            f'STALE ANCHOR {name}: {count} matches in {relative}'
            for name, relative, count in stale) + '\n'
        (out / 'stale-anchors.log').write_text(report)
        print(report, end='', flush=True)
        print('STALE ANCHORS: negative.py is out of sync with envelope.rs; refusing to run',
              flush=True)
        sys.exit(2)
    baseline_ok = False
    baseline = {'name': 'baseline', 'outcome': 'INVALID', 'returncode': None}
    try:
        try:
            slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
            slots_text = slots.stdout + slots.stderr
            run = run_owned(command, cwd=root, env=env, timeout=600)
            log = slots_text + run.stdout + run.stderr
            returncode = run.returncode
        except subprocess.TimeoutExpired as error:
            log = (error.stdout or '') + (error.stderr or '')
            returncode = None
        (out / 'baseline.log').write_text(log)
        passed = re.search(r'^test result: ok\. \d+ passed; 0 failed;', log, re.MULTILINE)
        baseline_ok = returncode == 0 and passed is not None
        baseline = {'name': 'baseline', 'outcome': 'PASS' if baseline_ok else 'FAIL',
                    'returncode': returncode}
        print(f'baseline: {"PASS" if baseline_ok else "FAIL"} exit={returncode}', flush=True)
        write_summary(baseline)
        if not baseline_ok:
            print('baseline failed; mutants not run', flush=True)
            raise SystemExit(1)

        for name, path, before, after, test in cases:
            slots_text = ''
            log = ''
            returncode = None
            try:
                slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
                slots_text = slots.stdout + slots.stderr
                # Snapshot bytes come only from mutating_sources; the mutation
                # itself is a single exact anchor replacement.
                path.write_bytes(original.replace(before, after).encode())
                try:
                    run = run_owned(
                        command + [test, '--', '--exact'],
                        cwd=root, env=env, timeout=600)
                    log = run.stdout + run.stderr
                    returncode = run.returncode
                finally:
                    path.write_bytes(originals[path])
            except subprocess.TimeoutExpired as error:
                log = (error.stdout or '') + (error.stderr or '')
                returncode = None
            except Exception as error:  # noqa: BLE001 - report, never hide a cell
                log = f'{type(error).__name__}: {error}\n'
                returncode = None
            outcome = classify(log, returncode)
            # A red baseline makes every mutation's "failure" untrustworthy.
            if outcome == 'KILLED' and not baseline_ok:
                outcome = 'INVALID'
            (out / f'{name}.log').write_text(
                'COMMAND ' + ' '.join(command + [test, '--', '--exact']) + '\n' +
                slots_text + log + f'\nexit={returncode}\n')
            results.append({'name': name, 'test': test, 'outcome': outcome,
                            'returncode': returncode, 'anchor': before, 'replacement': after})
            write_summary(baseline)
            print(f'{name}: {outcome} exit={returncode}', flush=True)
    finally:
        for path, data in originals.items():
            path.write_bytes(data)
            assert path.read_bytes() == data, 'restoration failed: ' + str(path)

killed = [row['name'] for row in results if row['outcome'] == 'KILLED']
survived = [row['name'] for row in results if row['outcome'] == 'SURVIVED']
invalid = [row['name'] for row in results if row['outcome'] == 'INVALID']
print(f'{len(results)} mutants: KILLED {len(killed)} / SURVIVED {len(survived)} / INVALID {len(invalid)}')
if survived:
    print('SURVIVED: ' + ', '.join(survived))
if invalid:
    print('INVALID: ' + ', '.join(invalid))
raise SystemExit(0 if baseline['outcome'] == 'PASS' and not survived and not invalid else 1)

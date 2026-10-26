#!/usr/bin/env python3
"""Reversible product mutations for edgeclaw-llm-envelope.

Every case must compile and fail a runtime oracle assertion; the script restores
the original bytes in `finally` and never touches oracle.json.

`envelope.rs` is a new file, so `mutating_sources` cannot read it from HEAD
until the commit exists (#764 rule 11). Run this from the host after commit:

    CARGO_TARGET_DIR=/abs/target python3 negative.py /tmp/edgeclaw-llm-envelope-negative
"""
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/llm/envelope.rs'
out = Path(sys.argv[1]).resolve()
target = os.environ.get('CARGO_TARGET_DIR')
assert target, 'negative.py requires CARGO_TARGET_DIR from the caller'
out.mkdir(parents=True, exist_ok=True)

cases = [
    # Drop the brace-bounded fast path: a multi-object response returns only the first object.
    ('object-shortcut', source,
     '    if trimmed[0] == OPEN_BRACE && trimmed[trimmed.len() - 1] == CLOSE_BRACE {',
     '    if trimmed[0] == OPEN_BRACE && trimmed[trimmed.len() - 1] == CLOSE_BRACE && false {',
     'json_object_matches_ts'),
    # Stop honouring backslash escapes while scanning; an escaped quote closes the string early.
    ('object-escape', source,
     '            } else if unit == BACKSLASH {\n                escaped = true;',
     '            } else if unit == BACKSLASH && false {\n                escaped = true;',
     'json_object_matches_ts'),
    # Envelope end-exclusive off-by-one drops the final `}`.
    ('envelope-slice', source,
     'Ok(JsString::from_utf16(trimmed[start..=end].to_vec()))',
     'Ok(JsString::from_utf16(trimmed[start..end].to_vec()))',
     'json_envelope_matches_ts'),
    # Skip the final double-backslash pass, so doubled backslashes survive.
    ('decode-final-unescape', source,
     '        (b"\\\\\\\\".as_slice(), b"\\\\".as_slice()),',
     '        (b"\\\\\\\\".as_slice(), b"\\\\\\\\".as_slice()),',
     'loose_string_matches_ts'),
    # Drop the CR-LF pass; CR then LF decode to a CR LF pair instead of LF.
    ('decode-crlf', source,
     '        (b"\\\\r\\\\n".as_slice(), b"\\n".as_slice()),',
     '        (b"\\\\r\\\\n".as_slice(), b"\\\\r\\\\n".as_slice()),',
     'loose_string_matches_ts'),
    # Lazy -> greedy capture: a duplicated next key extends the string group.
    ('string-lazy-capture', source,
     '    (start..units.len()).find(|&end| units[end] == QUOTE && comma_key_at(units, end + 1, next_keys))',
     '    (start..units.len()).rev().find(|&end| units[end] == QUOTE && comma_key_at(units, end + 1, next_keys))',
     'loose_string_matches_ts'),
    # Treat every negative slice bound as zero: `slice(0, -2)` loses its tail.
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
    ('unique-dedupe', source,
     '        if seen.insert(value.clone()) {\n            out.push(value);',
     '        if seen.insert(value.clone()) || true {\n            out.push(value);',
     'string_arrays_match_ts'),
    # Route matching is case-sensitive; accept `USER` and the oracle flips.
    ('route-case', source,
     'JsValue::String(text) if text.equals_str("user") => MemoryRoute::User,',
     'JsValue::String(text) if text.equals_str("user") || text.equals_str("USER") => MemoryRoute::User,',
     'route_and_boolean_match_ts'),
    # Drop `toLowerCase`: `"TRUE"` no longer parses as true.
    ('boolean-case', source,
     '            let normalized = lower(&trimmed, &LanguageIdentifier::UNKNOWN, false);',
     '            let normalized = trimmed;',
     'route_and_boolean_match_ts'),
    # The loose payload requires all three strings; dropping markdown accepts
    # payloads that TS rejects as null.
    ('payload-required', source,
     'if payload.name.is_some() && payload.description.is_some() && payload.markdown.is_some() {',
     'if payload.name.is_some() && payload.description.is_some() {',
     'loose_payload_matches_ts'),
    # Swallow the envelope error into `null`: TS throws for empty/no-brace text.
    ('payload-error', source,
     '    let envelope = extract_loose_json_envelope(raw)?;',
     '    let Ok(envelope) = extract_loose_json_envelope(raw) else { return Ok(None) };',
     'loose_payload_matches_ts'),
    # The greedy group's closing quote must have only JS whitespace before the
    # final `}`; without that check `{"markdown":"x"} }` parses.
    ('string-greedy-gap', source,
     '.any(|&unit| !js_is_space_unit(unit))',
     '.any(|&unit| unit == u16::MAX)',
     'loose_string_matches_ts'),
    # Do not consume the trailing JS whitespace before `$`: a final newline
    # turns a valid markdown capture into no capture.
    ('string-greedy-trailing', source,
     '    while end > 0 && js_is_space_unit(units[end - 1]) {',
     '    while end > 0 && units[end - 1] == u16::MAX {',
     'loose_string_matches_ts'),
    # truncateForPrompt must normalize before truncating; whitespace survives.
    ('prompt-normalize', source,
     '    truncate(&normalize_whitespace(value), max_length)',
     '    truncate(value, max_length)',
     'truncation_matches_ts'),
    # Non-unicode `/i` folds ASCII; without folding `"SKIP"` misses.
    ('boolean-ascii-fold', source,
     '        (true, true) => ascii_upper(left) == ascii_upper(right),',
     '        (true, true) => left == right,',
     'loose_boolean_matches_ts'),
    # Non-ASCII pairs fold through their upper-case mapping; `"é"` != `"É"`.
    ('boolean-nonascii-fold', source,
     '        (false, false) => simple_upper(left) == simple_upper(right),',
     '        (false, false) => false,',
     'loose_boolean_matches_ts'),
]

env = dict(os.environ)
env['CARGO_TARGET_DIR'] = target
with mutating_sources(root, [source]) as originals:
    try:
        for name, path, before, after, test in cases:
            original = originals[path].decode()
            assert original.count(before) == 1, (name, original.count(before))
            slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
            path.write_text(original.replace(before, after))
            try:
                cmd = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
                       '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_llm_envelope',
                       test, '--', '--exact']
                run = run_owned(cmd, cwd=root, env=env, timeout=600)
                log = run.stdout + run.stderr
                (out / f'{name}.log').write_text(slots.stdout + slots.stderr + log)
                failed = 'test result: FAILED.' in log
                assert run.returncode != 0 and failed and 'could not compile' not in log, \
                    name + ' did not fail a runtime assertion'
                lines = [line for line in log.splitlines()
                         if 'failed:' in line or 'panicked at' in line or 'test result:' in line]
                print(name + ': ' + ' | '.join(lines), flush=True)
            finally:
                path.write_bytes(originals[path])
    finally:
        for path, data in originals.items():
            path.write_bytes(data)
            assert path.read_bytes() == data, 'restoration failed: ' + str(path)

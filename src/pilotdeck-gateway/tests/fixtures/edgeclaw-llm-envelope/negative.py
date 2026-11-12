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
import itertools
import json
import os
from pathlib import Path
import re
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed
from negative_process import (detach_dead_streams, mutating_sources,
                              run_owned, unique_summary_tmp,
                              wait_for_slot, write_source_atomic)

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/llm/envelope.rs'

# Per-invocation serial for the label-log run token below.
_RUN_SERIAL = itertools.count(1)

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

# #1087 fix-r2 (b-tier P1, review P2#5): every kill is now bound to the
# failing oracle case's own assertion. The replay driver prints
# ``{fn} args={:?} clock={clock_ms}`` (or ``{fn} args={:?}`` on the throw
# shape) as the assert_eq! custom message, so the TS function name that
# opens the message is the anchor. Groups whose oracle mixes several
# functions (arrays / truncate / route) carry the alternatives of that
# group; named_test_killed accepts an iterable of alternatives.
EXPECTED = {
    'object-shortcut': 'extractFirstJsonObject args=',
    'object-escape': 'extractFirstJsonObject args=',
    'envelope-slice': 'extractLooseJsonEnvelope args=',
    'decode-final-unescape': 'extractLooseJsonStringProperty args=',
    'decode-crlf': 'extractLooseJsonStringProperty args=',
    'string-lazy-capture': 'extractLooseJsonStringProperty args=',
    'slice-negative': ('truncate args=', 'normalizeWhitespace args=',
                       'truncateForPrompt args='),
    'unique-dedupe': ('normalizeStringArray args=', 'uniqueStrings args='),
    'route-case': ('normalizeMemoryRoute args=', 'normalizeBoolean args='),
    'boolean-case': ('normalizeMemoryRoute args=', 'normalizeBoolean args='),
    'payload-required': 'tryParseLooseMemoryCreatePayload args=',
    'payload-error': 'tryParseLooseMemoryCreatePayload args=',
    'string-greedy-gap': 'extractLooseJsonStringProperty args=',
    'string-greedy-trailing': 'extractLooseJsonStringProperty args=',
    'prompt-normalize': ('truncate args=', 'normalizeWhitespace args=',
                         'truncateForPrompt args='),
    'boolean-ascii-fold': 'extractLooseJsonBooleanProperty args=',
    'boolean-nonascii-fold': 'extractLooseJsonBooleanProperty args=',
    'array-single-string': ('normalizeStringArray args=', 'uniqueStrings args='),
    'array-single-blank': ('normalizeStringArray args=', 'uniqueStrings args='),
    'array-nonstring-filter': ('normalizeStringArray args=',
                               'uniqueStrings args='),
    'array-blank-filter': ('normalizeStringArray args=', 'uniqueStrings args='),
    'array-non-array': ('normalizeStringArray args=', 'uniqueStrings args='),
    'string-empty-capture': 'extractLooseJsonStringProperty args=',
    'boolean-false-literal': 'extractLooseJsonBooleanProperty args=',
    'key-case-fold': 'extractLooseJsonBooleanProperty args=',
    'comma-next-key-filter': 'extractLooseJsonStringProperty args=',
}


def classify(log, returncode, expected, test):
    # #1087 fix-r2: the kill criterion is the shared strict #813 judge — the
    # named test's own FAILED status line plus the bound ``<fn> args=`` anchor
    # inside that test's own panic payload (negative_panic judges both). The
    # old shape only checked ``test result: FAILED.`` plus ``running 1
    # test``: any failure (an unrelated panic, a fixture defect) read as
    # KILLED. Defined before the argv dispatch so the twin controls in
    # test_negative.py can import the module and drive it directly.
    if returncode is None:
        return 'INVALID'
    if 'could not compile' in log:
        return 'INVALID'
    if returncode != 0 and 'test result: FAILED.' in log and 'running 1 test' in log:
        if named_test_killed(log, test, expected):
            return 'KILLED'
        return 'INVALID'
    if returncode == 0 and 'test result: ok.' in log:
        return 'SURVIVED'
    return 'INVALID'


if sys.argv[1:] == ['--list']:
    print('\n'.join(names))
    raise SystemExit(0)
out = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else None


def _refuse_summary(reason):
    """Invalidate a reused directory's stale summary (#1087 fix-r4, concurrency #3).

    The evidence directory is claimed with ``mkdir(exist_ok=True)`` so a host
    re-run can land in the same path; an exit-2 "never started" refusal
    must not leave the previous run's green ``summary.json`` readable
    there as this run's evidence. The marker is one rename, carries no
    verdict rows, and leaves a fresh directory untouched — before the first
    run there is nothing to invalidate.

    #1087 fix-r5 (boundary #3): a failing marker write must not turn the
    refusal into a traceback (rc=1, "ran and was red") — the reason is
    reported best-effort and the caller keeps its exit-2 code either way.
    """
    if out is None or not (out / 'summary.json').exists():
        return
    try:
        tmp = unique_summary_tmp(out)
        tmp.write_text(json.dumps({'refused': reason}, indent=2) + '\n')
        tmp.replace(out / 'summary.json')
    except OSError as error:
        try:
            print(f'could not invalidate the reused summary: {error}',
                  file=sys.stderr)
        except (OSError, ValueError):
            pass


def _refuse(message):
    """#1087 fix-r5 (concurrency #1): an early check is a refusal, not a crash.

    The old ``assert`` traceback exited 1 — "ran and was red" — and left a
    reused directory's previous green summary readable as this run's
    evidence. Now the stale summary is invalidated by one marker rename and
    the run exits 2 ("never started"), the same contract the in-window
    stale-anchor refusal keeps.
    """
    _refuse_summary(f'{message}: run refused')
    try:
        print(f'INVALID: {message}', file=sys.stderr)
    except (OSError, ValueError):
        # #1087 fix-r5 (boundary #1): stderr may be dead (`2>&-` gives
        # sys.stderr=None, a closed stream raises); the code is the contract.
        pass
    raise SystemExit(2)


if out is None:
    _refuse('usage: negative.py OUT_DIR [NAME ...] | negative.py --list')
selected = sys.argv[2:]
unknown = set(selected) - set(names)
if unknown:
    _refuse(f'unknown controls: {sorted(unknown)}')
if selected:
    cases = [case for case in cases if case[0] in selected]
if not cases:
    _refuse('no controls selected')

target = os.environ.get('CARGO_TARGET_DIR')
if not target:
    _refuse('negative.py requires CARGO_TARGET_DIR from the caller')
out.mkdir(parents=True, exist_ok=True)
run_token = f'{os.getpid()}.{next(_RUN_SERIAL)}'

env = dict(os.environ)
env['CARGO_TARGET_DIR'] = target
command = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
           '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_llm_envelope']
results = []
# #1087 fix-r3 (boundary #1): bound before the guarded window so the
# cancellation handler can always publish, even when the abort happened
# before the baseline run recorded anything.
baseline = {'name': 'baseline', 'outcome': 'INVALID', 'returncode': None}


def write_summary(current_baseline):
    """Publish the one final summary atomically (#1087 fix-r3, boundary #1).

    Called exactly once per run, after ``mutating_sources`` restored and
    byte-verified every guarded source — or from the cancellation handler,
    after the same guard unwound. The old per-row truncating ``write_text``
    left a partial all-KILLED summary on disk when a cancellation killed the
    run mid-suite, and an interrupt inside the write could leave an empty or
    half-written JSON document.
    """
    tmp = unique_summary_tmp(out)
    tmp.write_text(json.dumps(
        {'baseline': current_baseline, 'cases': results}, indent=2) + '\n')
    tmp.replace(out / 'summary.json')


def _void_pending(error):
    """Void the recorded rows and publish one INVALID summary (boundary #1).

    A cancellation or crash inside the guarded window is not a verdict: the
    rows recorded so far are voided, the remaining selected cases are
    recorded INVALID, and the single summary is published after the guard
    restored the tree. The process then exits 3 — deliberately different
    from 2 ("never started", the stale-anchor refusal below) so a reader can
    tell a cancelled run from one that never ran.
    """
    reason = f'runner: {type(error).__name__}: {error}'
    for row in results:
        row['outcome'] = 'INVALID'
        row['reason'] = (f"{row['reason']}; voided: {reason}"
                         if row['reason'] else f"voided: {reason}")
    for name, _path, before, after, test in cases[len(results):]:
        results.append({'name': name, 'test': test, 'outcome': 'INVALID',
                        'reason': reason, 'returncode': None,
                        'anchor': before, 'replacement': after})
    write_summary(baseline)
    try:
        print(f'voided {len(results)} row(s): {reason}', file=sys.stderr, flush=True)
    except (OSError, ValueError):
        # #1087 fix-r4 (concurrency #2): the summary is already published, so
        # a closed stderr must not replace the exit code below — "aborted = 3"
        # is the only contract left to keep. #1087 fix-r5: ValueError covers
        # an in-process closed stream, OSError the real dead fd.
        pass
    if isinstance(error, (OSError, ValueError)):
        # #1087 fix-r5 (concurrency #2): a stream-failure abort detaches the
        # dead fds first, so the interpreter-shutdown flush cannot rewrite
        # the exit code below to 120 ("Exception ignored in:").
        detach_dead_streams()
    if isinstance(error, SystemExit):
        # Normalize: 128+signum and any other raw code must not decide how a
        # coordinator reads this run (a 256/-256 code truncates to a zero
        # process status on POSIX).
        raise SystemExit(3) from error
    if not isinstance(error, Exception):
        raise
    raise SystemExit(3) from error



try:
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
            (out / f'stale-anchors.{run_token}.log').write_text(report)
            try:
                print(report, end='', flush=True)
                print('STALE ANCHORS: negative.py is out of sync with envelope.rs; refusing to run',
                      flush=True)
            except (OSError, ValueError):
            # #1087 fix-r5 (boundary #1 family): a dead stdout must not
            # turn this exit-2 refusal into an in-window abort (the
            # report is already on disk in the stale-anchors log).
                pass
            sys.exit(2)
        baseline_ok = False
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
            (out / f'baseline.{run_token}.log').write_text(log)
            passed = re.search(r'^test result: ok\. \d+ passed; 0 failed;', log, re.MULTILINE)
            baseline_ok = returncode == 0 and passed is not None
            baseline = {'name': 'baseline', 'outcome': 'PASS' if baseline_ok else 'FAIL',
                        'returncode': returncode}
            print(f'baseline: {"PASS" if baseline_ok else "FAIL"} exit={returncode}', flush=True)
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
                    # #1087 fix-r2: every guarded-source write in the window is
                    # one atomic rename (#1903 rule, shared with
                    # ``mutating_sources``) — a truncating ``write_bytes`` killed
                    # mid-write leaves an empty or half-written product source.
                    write_source_atomic(root, path,
                                         original.replace(before, after).encode(),
                                         mode=path.stat().st_mode & 0o7777)
                    try:
                        run = run_owned(
                            command + [test, '--', '--exact'],
                            cwd=root, env=env, timeout=600)
                        log = run.stdout + run.stderr
                        returncode = run.returncode
                    finally:
                        # Same #1087 fix-r2 window: the in-loop restore is also
                        # one rename, never a truncating write_bytes.
                        write_source_atomic(root, path, originals[path],
                                            mode=path.stat().st_mode & 0o7777)
                except subprocess.TimeoutExpired as error:
                    log = (error.stdout or '') + (error.stderr or '')
                    returncode = None
                except Exception as error:  # noqa: BLE001 - report, never hide a cell
                    log = f'{type(error).__name__}: {error}\n'
                    returncode = None
                outcome = classify(log, returncode, EXPECTED[name], test)
                # A red baseline makes every mutation's "failure" untrustworthy.
                if outcome == 'KILLED' and not baseline_ok:
                    outcome = 'INVALID'
                (out / f'{name}.{run_token}.log').write_text(
                    'COMMAND ' + ' '.join(command + [test, '--', '--exact']) + '\n' +
                    slots_text + log + f'\nexit={returncode}\n')
                results.append({'name': name, 'test': test, 'outcome': outcome,
                                'reason': '', 'returncode': returncode,
                                'anchor': before, 'replacement': after})
                print(f'{name}: {outcome} exit={returncode}', flush=True)
        finally:
            for path, data in originals.items():
                # Same #1087 fix-r2 window: the final restore is also one rename,
                # never a truncating write_bytes.
                write_source_atomic(root, path, data,
                                    mode=path.stat().st_mode & 0o7777)
except SystemExit as error:
    if error.code == 1:
        # The deliberate baseline failure: the guard already restored and
        # byte-verified the tree while unwinding the with-block; publish the
        # baseline row and keep the "ran and failed" exit code.
        write_summary(baseline)
        raise
    if error.code == 2:
        # The deliberate stale-anchor refusal: nothing was measured, so
        # nothing is published. The directory claim above allows a host
        # re-run into the same path, so a previous run's green summary is
        # invalidated before the refusal propagates — it must not stay
        # readable as this run's evidence (#1087 fix-r4, concurrency #3).
        _refuse_summary('stale anchors: negative.py is out of sync with its sources')
        raise
    _void_pending(error)
except BaseException as error:  # noqa: BLE001 - void, publish, then re-raise
    _void_pending(error)
# Reaching here means mutating_sources restored every guarded file and
# verified it against the baseline bytes. Only now may a summary exist.
write_summary(baseline)

killed = [row['name'] for row in results if row['outcome'] == 'KILLED']
survived = [row['name'] for row in results if row['outcome'] == 'SURVIVED']
invalid = [row['name'] for row in results if row['outcome'] == 'INVALID']
print(f'{len(results)} mutants: KILLED {len(killed)} / SURVIVED {len(survived)} / INVALID {len(invalid)}')
if survived:
    print('SURVIVED: ' + ', '.join(survived))
if invalid:
    print('INVALID: ' + ', '.join(invalid))
raise SystemExit(0 if baseline['outcome'] == 'PASS' and not survived and not invalid else 1)

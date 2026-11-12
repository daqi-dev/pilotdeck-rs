#!/usr/bin/env python3
"""Reversible product mutations for edgeclaw-llm-config.

Every case must compile and fail a runtime oracle assertion. Every anchor is
checked against the baseline bytes before the unmutated suite runs: a stale
anchor is a harness defect, not a surviving mutant, so it prints
``STALE ANCHOR`` and exits 2 (same rule as #772 fix-r2). A surviving case
does not stop the run: all selected cases execute, the script prints a
KILLED / SURVIVED / INVALID summary, and exits 1 when anything survived or is
invalid. The original bytes come from ``mutating_sources`` and are restored
after every case and again in the outer ``finally``; oracle.json is untouched.

Run all cases (the fixture is committed, so ``mutating_sources`` can read it):

    CARGO_TARGET_DIR=/abs/target python3 negative.py /tmp/edgeclaw-negative

Run only the name-match sample plus one control, or list the names:

    CARGO_TARGET_DIR=/abs/target python3 negative.py /tmp/edgeclaw-negative \
        model-name-match provider-default
    python3 negative.py --list
    python3 negative.py --self-test

The ``# oracle`` comments name the ``cases[i]`` index whose expected value
flips; the RESULT for fix-r2 lists every witness. ``--self-test`` checks the
three-way classifier directly and needs no Cargo target directory.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed
from negative_process import (detach_dead_streams, mutating_sources,
                              run_owned, wait_for_slot,
                              write_source_atomic)
from negative_selection import (claim_dir, missing_target, refuse,
                               refuse_unusable_target, select)


def classify(log, returncode, expected, test):
    """Three-way outcome for one mutant run.

    Every KILLED/SURVIVED verdict must prove that exactly one test actually
    ran. A renamed or filtered-out test prints ``running 0 tests`` and exits 0;
    without this guard it would be reported as SURVIVED (a false negative).
    #1087 fix-r2 (b-tier P1, review P2#5): a KILLED verdict additionally
    requires the shared strict #813 judge — the named test's own FAILED
    status line plus the bound anchor inside that test's own panic payload
    (negative_panic judges both). The old shape accepted any failure,
    including an unrelated panic in the same binary — the false-kill shapes
    ``--self-test`` and the twin controls in test_negative.py pin.
    """
    if returncode is None:
        return 'INVALID'
    if 'could not compile' in log:
        return 'INVALID'
    ran = re.search(r'^running (\d+) tests?$', log, re.MULTILINE)
    if ran is None or int(ran.group(1)) != 1:
        return 'INVALID'
    if returncode != 0 and 'test result: FAILED.' in log:
        if named_test_killed(log, test, expected):
            return 'KILLED'
        return 'INVALID'
    if returncode == 0 and 'test result: ok.' in log:
        return 'SURVIVED'
    return 'INVALID'


def self_test():
    anchor = '$'
    test = 't_selftest'
    killed_log = (
        'running 1 test\n'
        f'test {test} ... FAILED\n\n'
        'failures:\n\n'
        f'---- {test} stdout ----\n'
        f"thread '{test}' panicked at tests/edgeclaw_llm_config.rs:90:13:\n"
        '$.provider: expected Some("p"), actual None\n'
        'note: run with `RUST_BACKTRACE=1` environment variable to display '
        'a backtrace\n\n'
        'failures:\n'
        f'    {test}\n\n'
        'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; '
        '0 filtered out\n'
    )
    # The same FAILED shape on an unrelated unwrap: the anchor is nowhere in
    # the test's own panic payload, so it is not a kill.
    unrelated_log = killed_log.replace(
        '$.provider: expected Some("p"), actual None',
        'called `Option::unwrap()` on a None value')
    samples = [
        ('running 0 tests\n\ntest result: ok. 0 passed; 0 failed;', 0, 'INVALID'),
        ('running 1 test\n\ntest result: ok. 1 passed; 0 failed;', 0, 'SURVIVED'),
        ('running 2 tests\n\ntest result: ok. 2 passed; 0 failed;', 0, 'INVALID'),
        (killed_log, 101, 'KILLED'),
        (unrelated_log, 101, 'INVALID'),
        (killed_log, 0, 'INVALID'),
        ('error: could not compile `pilotdeck-gateway`', 101, 'INVALID'),
        ('running 0 tests\n\ntest result: FAILED.', 101, 'INVALID'),
    ]
    for log, returncode, expected in samples:
        actual = classify(log, returncode, anchor, test)
        if actual != expected:
            raise RuntimeError((log, returncode, expected, actual))
    print('classify self-test: PASS')


root = Path(__file__).resolve().parents[5]
config = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/llm/config.rs'

cases = [
    # Default provider: the last env fallback disappears.
    # oracle: buildLlmConfig #3 provider `edgeclaw_memory` -> `""`.
    ('provider-default', config,
     '        JsString::from("edgeclaw_memory"),\n', '        JsString::from(""),\n',
     'llm_config_matches_ts'),
    # First failure must stay cached: make a successful read cache `null`.
    # oracle: cache #2 valid-then-other expected [config,config] -> [config,null].
    ('openclaw-cache-null', config,
     '        *cached = Some(value.clone());\n        value\n',
     '        *cached = Some(None);\n        value\n',
     'llm_config_matches_ts'),
    # Model selection must also match `name`, not only `id`.
    # oracle: #29 `alpha/friendly` selects the second model; api/baseUrl/headers flip.
    ('model-name-match', config,
     'field(Some(entry), "id") == *model || field(Some(entry), "name") == *model',
     'field(Some(entry), "id") == *model',
     'llm_config_matches_ts'),
    # `EDGECLAW_MEMORY_MODEL` must feed the modelRef parser.
    # oracle: buildLlmConfig #4 env-chain `env-model` -> OPENAI_MODEL fallback.
    ('env-model-ref', config,
     '        env_trim(env, "EDGECLAW_MEMORY_MODEL"),\n', '        JsString::default(),\n',
     'llm_config_matches_ts'),
    # Selection baseUrl strips trailing slashes.
    # oracle: resolveSelection #158/#159 `https://m.example` -> `...//`;
    # #162/#163 `/` -> `/`.
    ('selection-trailing-slash', config,
     'value.units()[end - 1] == u16::from(b\'/\')', 'value.units()[end - 1] == u16::from(b\' \')',
     'llm_config_matches_ts'),
    # Environment-shaped apiKey values expand from the injected env.
    # oracle: resolveApiKey #196 `from-env` -> `ENV_KEY`.
    ('api-key-env', config,
     '        if looks_like_env_var_name(&configured)\n', '        if false && looks_like_env_var_name(&configured)\n',
     'llm_config_matches_ts'),
    # `Math.floor` on the interval, not ceiling.
    # oracle: mergeIndexingSettings #305 autoDream 2.9 -> 3.
    ('merge-floor', config,
     '            let clamped = value.floor().max(0.0);',
     '            let clamped = value.ceil().max(0.0);',
     'llm_config_matches_ts'),
    # fix-r5 P2-3: only the new `-0` input row distinguishes this mutation;
    # `+0`/negative floors still return `+0`, so an earlier merge row cannot
    # absorb the kill. The oracle expected value is `+0` (`Math.max(0, -0)`).
    ('merge-negative-zero', config,
     '            if clamped == 0.0 { 0.0 } else { clamped }',
     '            if clamped == 0.0 {\n'
     '                if *value == 0.0 && value.is_sign_negative() { -0.0 } else { 0.0 }\n'
     '            } else { clamped }',
     'llm_config_matches_ts'),
    # Header values are stored untrimmed.
    # oracle: sanitizeHeaders #37 `"  keep  "` -> `"keep"`; resolveSelection
    # #158 keeps model header `" m "` raw.
    ('sanitize-header-value', config,
     '            out.insert(key, JsValue::String(text.clone()));',
     '            out.insert(key, JsValue::String(trimmed(Some(text))));',
     'llm_config_matches_ts'),
    # Model headers override provider headers.
    # oracle: resolveSelection #158/#159 `P` keeps the model value `" m "`.
    ('headers-model-wins', config,
     '''    for source in [
        provider.and_then(|provider| provider.get_str("headers")),
        model.and_then(|model| model.get_str("headers")),
    ]''',
     '''    for source in [
        model.and_then(|model| model.get_str("headers")),
        provider.and_then(|provider| provider.get_str("headers")),
    ]''',
     'llm_config_matches_ts'),
    # The apiType default is `openai-responses`.
    # oracle: buildLlmConfig #3 `openai-responses` -> `anthropic`.
    ('api-type-default', config,
     '        JsString::from("openai-responses"),\n', '        JsString::from("anthropic"),\n',
     'llm_config_matches_ts'),
    # NaN must stay NaN: `JSON.stringify` writes both NaN and Infinity as
    # `null`, so only the strict number comparator can kill this.
    # oracle: mergeIndexingSettings #308 autoIndex NaN -> Infinity.
    ('merge-nan', config,
     'Some(JsValue::Number(value)) if value.is_nan() => f64::NAN,',
     'Some(JsValue::Number(value)) if value.is_nan() => f64::INFINITY,',
     'llm_config_matches_ts'),
    # Available providers follow ECMAScript own-key order (integer keys
    # ascending), not JSON text order.
    # oracle: openclaw #363 `10`/`2` text order, primary miss -> provider `2`.
    ('provider-key-order', config,
     '''    let available: Vec<(&JsString, &JsObject)> = providers
        .own_entries()
        .into_iter()''',
     '''    let available: Vec<(&JsString, &JsObject)> = providers
        .entries()
        .iter()
        .map(|(name, value)| (name, value))''',
     'llm_config_matches_ts'),
    # `toStringRecord` rejects arrays; accepting them leaks array indices as
    # header names.
    # oracle: openclaw #361/#362 array headers are dropped.
    ('headers-array-accepted', config,
     '''fn to_string_record(value: Option<&JsValue>) -> Option<JsObject> {
    let record = value.and_then(JsValue::as_object)?;
    let mut out = JsObject::new();
    for (key, entry) in record.own_entries() {
        let text = trimmed(entry.as_string());
        if !text.is_empty() {
            out.insert(key.clone(), JsValue::String(text));
        }
    }
    (!out.is_empty()).then_some(out)
}''',
     '''fn to_string_record(value: Option<&JsValue>) -> Option<JsObject> {
    let mut out = JsObject::new();
    for (key, entry) in JsRecord::of(value?)?.entries() {
        let text = trimmed(entry.as_string());
        if !text.is_empty() {
            out.insert(key, JsValue::String(text));
        }
    }
    (!out.is_empty()).then_some(out)
}''',
     'llm_config_matches_ts'),
    # `resolveMemoryLlm` keeps `cfg.model` untrimmed.
    # oracle: resolveMemoryLlm #364 ` alpha/m1 ` -> provider ` alpha`.
    ('memory-model-trim', config,
     '''    let model_ref = config
        .model
        .clone()''',
     '''    let model_ref = config
        .model
        .clone()
        .map(|value| trimmed(Some(&value)))''',
     'llm_config_matches_ts'),
    # The cache lock must not cover home probing / file I/O; restoring the
    # lock-held body makes the concurrency test time out.
    ('lock-held-io', config,
     '''    pub fn resolve(&self, home: impl FnOnce() -> Option<PathBuf>) -> Option<OpenClawModelConfig> {
        if let Some(value) = self.read_cache() {
            return value;
        }
        let value =
            home().and_then(|home| load_open_claw_model_config(&open_claw_config_path(&home)));
        let mut cached = self
            .cached
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(value) = cached.as_ref() {
            return value.clone();
        }
        *cached = Some(value.clone());
        value
    }''',
     '''    pub fn resolve(&self, home: impl FnOnce() -> Option<PathBuf>) -> Option<OpenClawModelConfig> {
        let mut cached = self
            .cached
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(value) = cached.as_ref() {
            return value.clone();
        }
        let value =
            home().and_then(|home| load_open_claw_model_config(&open_claw_config_path(&home)));
        *cached = Some(value.clone());
        value
    }''',
     'open_claw_resolve_does_not_hold_lock_across_io'),
    # #851 P1-1: sanitizeHeaders' plain `next[key] = value` must not turn a
    # JSON.parse-built own `__proto__` into an own property (a plain
    # assignment routes the key to the Object.prototype setter).
    # oracle: sanitizeHeaders #366 keeps only `Authorization`, #367 only-proto
    # is undefined; resolveSelection #368/#369 headers drop `__proto__`.
    ('sanitize-proto-kept', config,
     '''            if !key.equals_str("__proto__") {
                out.insert(key, JsValue::String(text.clone()));
            }''',
     '''            {
                out.insert(key, JsValue::String(text.clone()));
            }''',
     'llm_config_matches_ts'),
    # #851 P1-2: `String(auth.apiKey)` must keep its conversion error instead
    # of coercing objects to "[object Object]".
    # oracle: resolveApiKey #372/#373 expect $error "Cannot convert object to
    # primitive value"; #374 plain object stays "[object Object]".
    ('api-key-unchecked-string', config,
     '''        let units =
            crate::js_value::to_string_units_checked(api_key).map_err(|error| ConfigError {
                message: JsString::from(error.message()),
            })?;''',
     '''        let units = crate::js_value::to_string_units(api_key).unwrap_or_default();''',
     'llm_config_matches_ts'),
    # #851 contrast: toStringRecord rebuilds with Object.fromEntries, which
    # defines own data properties, so it must KEEP an own `__proto__` —
    # unifying it with sanitizeHeaders drops the key.
    # oracle: resolveOpenClawModelConfig #371 and buildLlmConfig #370 headers
    # keep `__proto__`.
    ('to-string-record-drops-proto', config,
     '''        let text = trimmed(entry.as_string());
        if !text.is_empty() {
            out.insert(key.clone(), JsValue::String(text));
        }''',
     '''        let text = trimmed(entry.as_string());
        if !text.is_empty() && !key.equals_str("__proto__") {
            out.insert(key.clone(), JsValue::String(text));
        }''',
     'llm_config_matches_ts'),
    # #836: the cached bad-file `null` must satisfy later callers; treating
    # that `Some(None)` as a miss re-probes home, which the #836 test's
    # panicking closure turns into the issue-specific failure message.
    ('openclaw-null-retry', config,
     '''        if let Some(value) = self.read_cache() {
            return value;
        }''',
     '''        if let Some(Some(value)) = self.read_cache() {
            return Some(value);
        }''',
     'open_claw_cache_bad_first_read_stays_null_without_reprobe'),
]

# #1087 fix-r2 (b-tier P1, review P2#5): a kill must be the named test
# failing on its own assertion. Every ``llm_config_matches_ts`` comparison
# goes through ``assert_same``/``assert_number``, whose failure messages all
# carry the ``$``-rooted JSON path of the first mismatch (the root call is
# ``assert_same(actual, expected, "$")``), so ``$`` is the shared class
# marker for that oracle replay. The three dedicated cells pin their own
# issue-specific panic messages instead. This table replaces the old
# ``KILL_FAILURE_TEXT`` whole-log pin: the anchors are now judged inside the
# named test's own panic payload by ``named_test_killed``.
EXPECTED = {
    'provider-default': '$',
    'openclaw-cache-null': '$',
    'model-name-match': '$',
    'env-model-ref': '$',
    'selection-trailing-slash': '$',
    'api-key-env': '$',
    'merge-floor': '$',
    'merge-negative-zero': '$.autoIndexIntervalMinutes: number bits',
    'sanitize-header-value': '$',
    'headers-model-wins': '$',
    'api-type-default': '$',
    'merge-nan': '$',
    'provider-key-order': '$',
    'headers-array-accepted': '$',
    'memory-model-trim': '$',
    'lock-held-io': 'second resolve blocked: the cache lock covers I/O',
    'sanitize-proto-kept': '$',
    'api-key-unchecked-string': '$',
    'to-string-record-drops-proto': '$',
    'openclaw-null-retry': '#836: cached bad-file null must not re-probe home',
}

names = [case[0] for case in cases]
if len(names) != len(set(names)):
    refuse('duplicate control name')
all_cases = list(cases)
# #1908 (from #1692 r15-4): ONE dispatch rule for every mode. Whole-argv
# equality let `--self-test extra` fall through to the main path, which
# treated `--self-test` as an output directory; an unknown `--flag` did the
# same and ran the whole table green. Every mode now takes an exact argument
# list, and only a real OUT_DIR reaches the main path.
USAGE = 'usage: negative.py OUT_DIR [NAME ...] | negative.py --list | negative.py --self-test'
mode, arguments = (sys.argv[1], sys.argv[2:]) if len(sys.argv) > 1 else (None, [])
if mode == '--list':
    if arguments:
        raise SystemExit(f'--list takes no arguments: {arguments!r}\n{USAGE}')
    print('\n'.join(names))
    raise SystemExit(0)
if mode == '--self-test':
    if arguments:
        raise SystemExit(f'--self-test takes no arguments: {arguments!r}\n{USAGE}')
    self_test()
    raise SystemExit(0)
# Explicit exits, not asserts: `python3 -O` strips asserts and a bare
# IndexError traceback is not a usage diagnostic. Zero arguments and an
# unknown `-*` first argument are both usage errors (non-zero exit).
if mode is None:
    raise SystemExit(USAGE)
if mode.startswith('-'):
    raise SystemExit(f'unknown mode: {mode}\n{USAGE}')
# Judgment checks are explicit exits, not asserts: python3 -O strips asserts
# (same hardening as the mode dispatch above), so a misspelled control name
# must not silently run zero mutants and exit 0. select() also refuses a
# repeated name (`a a` for `a b`) instead of folding it into a green run.
try:
    run_rows = select(all_cases, arguments, always=())
except ValueError as error:
    refuse(str(error))
if not run_rows:
    refuse('no controls selected')

target = os.environ.get('CARGO_TARGET_DIR')
if missing_target():
    refuse('set CARGO_TARGET_DIR to the caller-owned build tree before running')
refuse_unusable_target()
out = claim_dir(mode)

env = dict(os.environ)
env['CARGO_TARGET_DIR'] = target
command = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
           '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_llm_config']
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
    tmp = out / 'summary.json.tmp'
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
    for name, _path, before, after, test in run_rows[len(results):]:
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
    with mutating_sources(root, [config]) as originals:
        original = originals[config].decode()
        # An anchor that this list did not follow is a harness defect, not a
        # classified cell: INVALID/exit 1 still blames the wrong thing and lets a
        # broken control hide behind an environment or compile fault. Refuse
        # before compiling.
        stale = []
        for name, _path, before, _after, _test in all_cases:
            count = original.count(before)
            if count != 1:
                stale.append((name, config.relative_to(root).as_posix(), count))
        if stale:
            report = '\n'.join(
                f'STALE ANCHOR {name}: {count} matches in {relative}'
                for name, relative, count in stale) + '\n'
            (out / 'stale-anchors.log').write_text(report)
            print(report, end='', flush=True)
            print('STALE ANCHORS: negative.py is out of sync with config.rs; refusing to run',
                  flush=True)
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
            (out / 'baseline.log').write_text(log)
            passed = re.search(r'^test result: ok\. \d+ passed; 0 failed;', log, re.MULTILINE)
            baseline_ok = returncode == 0 and passed is not None
            baseline = {'name': 'baseline', 'outcome': 'PASS' if baseline_ok else 'FAIL',
                        'returncode': returncode}
            print(f'baseline: {"PASS" if baseline_ok else "FAIL"} exit={returncode}', flush=True)
            if not baseline_ok:
                print('baseline failed; mutants not run', flush=True)
                raise SystemExit(1)

            for name, path, before, after, test in run_rows:
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
                (out / f'{name}.log').write_text(
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
                if path.read_bytes() != data:
                    raise RuntimeError('restoration failed: ' + str(path))
except SystemExit as error:
    if error.code == 1:
        # The deliberate baseline failure: the guard already restored and
        # byte-verified the tree while unwinding the with-block; publish the
        # baseline row and keep the "ran and failed" exit code.
        write_summary(baseline)
        raise
    if error.code == 2:
        # The deliberate stale-anchor refusal: nothing was measured, so
        # nothing is published — "never started" leaves no summary behind.
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


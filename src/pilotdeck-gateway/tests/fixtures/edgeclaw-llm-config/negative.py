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
from negative_process import mutating_sources, run_owned, wait_for_slot


def classify(log, returncode):
    """Three-way outcome for one mutant run.

    Every KILLED/SURVIVED verdict must prove that exactly one test actually
    ran. A renamed or filtered-out test prints ``running 0 tests`` and exits 0;
    without this guard it would be reported as SURVIVED (a false negative).
    """
    if returncode is None:
        return 'INVALID'
    if 'could not compile' in log:
        return 'INVALID'
    ran = re.search(r'^running (\d+) tests?$', log, re.MULTILINE)
    if ran is None or int(ran.group(1)) != 1:
        return 'INVALID'
    if returncode != 0 and 'test result: FAILED.' in log:
        return 'KILLED'
    if returncode == 0 and 'test result: ok.' in log:
        return 'SURVIVED'
    return 'INVALID'


def self_test():
    samples = [
        ('running 0 tests\n\ntest result: ok. 0 passed; 0 failed;', 0, 'INVALID'),
        ('running 1 test\n\ntest result: ok. 1 passed; 0 failed;', 0, 'SURVIVED'),
        ('running 2 tests\n\ntest result: ok. 2 passed; 0 failed;', 0, 'INVALID'),
        ('running 1 test\n\ntest result: FAILED. 0 passed; 1 failed;', 101, 'KILLED'),
        ('running 1 test\n\ntest result: FAILED. 0 passed; 1 failed;', 0, 'INVALID'),
        ('error: could not compile `pilotdeck-gateway`', 101, 'INVALID'),
        ('running 0 tests\n\ntest result: FAILED.', 101, 'INVALID'),
    ]
    for log, returncode, expected in samples:
        actual = classify(log, returncode)
        assert actual == expected, (log, returncode, expected, actual)
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
     'Some(JsValue::Number(value)) => value.floor().max(0.0)',
     'Some(JsValue::Number(value)) => value.ceil().max(0.0)',
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
]

names = [case[0] for case in cases]
assert len(names) == len(set(names)), 'duplicate control name'
all_cases = list(cases)
if sys.argv[1:] == ['--list']:
    print('\n'.join(names))
    raise SystemExit(0)
if sys.argv[1:] == ['--self-test']:
    self_test()
    raise SystemExit(0)
assert len(sys.argv) >= 2, 'usage: negative.py OUT_DIR [NAME ...] | negative.py --list | negative.py --self-test'
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
           '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_llm_config']
results = []


def write_summary(baseline):
    (out / 'summary.json').write_text(json.dumps(
        {'baseline': baseline, 'cases': results}, indent=2) + '\n')


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

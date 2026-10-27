#!/usr/bin/env python3
"""Reversible product mutations for #767 ReasoningRetriever.

Every case must compile and fail the bound ``#767 ...`` assertion in the named
test. Anchors are checked against the baseline bytes before anything runs: a
stale anchor is a harness defect, so it prints ``STALE ANCHOR`` and exits 2
(never SURVIVED). A surviving case does not stop the run; every selected case
executes, then the script prints a KILLED / SURVIVED / INVALID summary and
exits 1 when anything is not KILLED.

``mutating_sources`` refuses to run unless the mutated sources match HEAD, so
use ``--check-anchors`` before committing changes and run the mutants once
those changes are committed:

    CARGO_TARGET_DIR=/abs/target python3 negative.py OUT_DIR
    CARGO_TARGET_DIR=/abs/target python3 negative.py OUT_DIR ttl-value
    python3 negative.py --check-anchors
    python3 negative.py --list

Cases carry a target: ``test`` runs ``--test edgeclaw_retriever`` (the replay
and M10 cells), while ``lib`` runs a ``#[cfg(test)]`` unit test in the retriever
module (the P1-1 atomic-commit guard). Both run one exact test so the bound
message is the one that has to fail.

Never run this in a shadow repository or alternate source tree. The gateway
product tree must be committed for the full run (the coordinator runs it after
this fixture lands); until then only ``--check-anchors`` is available here.
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
retriever = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/retrieval/retriever.rs'
tests = root / 'src/pilotdeck-gateway/tests/edgeclaw_retriever.rs'

cases = [
    # The retained recent user-message contents are part of the cache key.
    # oracle: m10_recent_key_miss distinguishes plain from recent-keyed calls.
    ('cache-key-recent', retriever,
     '''        (
            "recent",
            JsValue::Array(
                recent
                    .iter()
                    .map(|message| JsValue::String(string_field(message, "content")))
                    .collect(),
            ),
        ),''',
     '''        ("recent", JsValue::Array(Vec::new())),''',
     'm10_recent_key_miss'),
    # The repository snapshot version is part of the cache key.
    # oracle: m10_snapshot_key_miss mutates the store between two calls.
    ('cache-key-snapshot', retriever,
     '        ("snapshot", JsValue::String(snapshot.clone())),',
     '        ("snapshot", JsValue::Null),',
     'm10_snapshot_key_miss'),
    # Query normalization collapses case and whitespace, so the second call
    # hits the first entry.
    # oracle: m10_cache_miss_hit_and_reset normalized hit.
    ('query-normalize', retriever,
     '        ("query", JsValue::String(normalize_query_key(query))),',
     '        ("query", JsValue::String(query.clone())),',
     'm10_cache_miss_hit_and_reset'),
    # The mode ("auto" vs "explicit") is part of the cache key.
    # oracle: replay single-routes/4 reuses the auto entry only if mode drops.
    ('mode-in-key', retriever,
     '''        ("mode", JsValue::String(mode.clone())),
        ("query", JsValue::String(normalize_query_key(query))),''',
     '''        ("mode", JsValue::String(s("auto"))),
        ("query", JsValue::String(normalize_query_key(query))),''',
     'replay_scenarios'),
    # RECALL_CACHE_TTL_MS is 30_000; 30_001 makes the exact boundary a hit.
    # oracle: m10_ttl_boundary expects a miss at expiresAt == now.
    ('ttl-value', retriever,
     'pub const RECALL_CACHE_TTL_MS: i64 = 30_000;',
     'pub const RECALL_CACHE_TTL_MS: i64 = 30_001;',
     'm10_ttl_boundary'),
    # TS requires `expiresAt > now`; `>=` keeps the boundary entry alive.
    # oracle: m10_ttl_boundary exact 30_000 boundary miss.
    ('ttl-comparison', retriever,
     '        if cached.expires_at <= now {',
     '        if cached.expires_at < now {',
     'm10_ttl_boundary'),
    # resetTransientState must clear the recall cache.
    # oracle: m10_cache_miss_hit_and_reset reset-clears cell.
    ('reset-not-clear', retriever,
     '        self.lock_state().recall_cache.clear();',
     '        let _ = self.lock_state().recall_cache.len();',
     'm10_cache_miss_hit_and_reset'),
    # The TS set happens after the LLM await; this mutant drops the set, so a
    # reset-during-await result is never cached.
    # oracle: m10_reset_during_await second call must be a cache hit.
    ('cache-never-set', retriever,
     '''        state.recall_cache.insert(
            key,
            RecallCacheEntry {
                expires_at: now.saturating_add(RECALL_CACHE_TTL_MS),
                result: result.clone(),
            },
        );''',
     '''        let _ = key;''',
     'm10_reset_during_await'),
    # Cache hits report elapsedMs = 0.
    # oracle: m10_cache_miss_hit_and_reset normalized hit.
    ('hit-elapsed', retriever,
     '    debug.insert(JsString::from("elapsedMs"), JsValue::Number(0.0));',
     '    debug.insert(JsString::from("elapsedMs"), JsValue::Number(1.0));',
     'm10_cache_miss_hit_and_reset'),
    # Cache hits overwrite lastRecallCacheHit (and keep recallTimeouts).
    # oracle: m10_cache_miss_hit_and_reset normalized-hit stats.
    ('hit-stats', retriever,
     '    stats.last_recall_cache_hit = true;',
     '    stats.last_recall_ms = 0.0;',
     'm10_cache_miss_hit_and_reset'),
    # Trace text must concatenate UTF-16 units; a UTF-8 round trip drops the
    # lone surrogate in the `mode=<mode>` output summary.
    # oracle: utf16_lone_surrogate_preserved outputSummary code units.
    ('utf16-concat', retriever,
     '            &concat_js(&[&s("mode="), &mode]),',
     '            &JsString::from(format!("mode={}", mode.to_utf8().unwrap_or_default())),',
     'utf16_lone_surrogate_preserved'),
    # P1-1: splitting the miss commit back into two lock acquisitions lets a
    # concurrent hit write `cacheHit=true` between the cache insert and the
    # stats write.
    # unit guard: the commit hook must still see the state mutex held.
    ('separate-commit', retriever,
     '''        #[cfg(test)]
        self.notify_commit_hook();
        let previous = state.runtime_stats.clone();''',
     '''        drop(state);
        #[cfg(test)]
        self.notify_commit_hook();
        let mut state = self.lock_state();
        let previous = state.runtime_stats.clone();''',
     'runtime::memory::edgeclaw::retriever::tests::commit_keeps_cache_and_stats_in_one_critical_section'),
    # P1-2: the old unconditional user path list re-adds the profile that TS
    # omits when `hasUserSummary` is false.
    # oracle: empty-user-profile/0 Injected Blocks.
    ('injected-blocks-user', retriever,
     '''        let mut blocks: Vec<JsString> = if has_user {
            user_paths.clone()
        } else {
            Vec::new()
        };''',
     '''        let mut blocks: Vec<JsString> = user_paths.clone();''',
     'replay_scenarios'),
    # P1-3: keeping the first cache entry makes the first completion win
    # instead of the later one; the controlled-order reverse cell catches it.
    ('same-key-or-insert', retriever,
     '''        state.recall_cache.insert(
            key,
            RecallCacheEntry {
                expires_at: now.saturating_add(RECALL_CACHE_TTL_MS),
                result: result.clone(),
            },
        );''',
     '''        state.recall_cache.entry(key).or_insert_with(|| RecallCacheEntry {
            expires_at: now.saturating_add(RECALL_CACHE_TTL_MS),
            result: result.clone(),
        });''',
     'm10_same_key_concurrent_reverse'),
    # P2: an off-by-one request ceiling must hit the bounded wait and print the
    # calling M10 cell's message (not hang the test suite).
    ('gated-wait-bound', tests,
     '            while self.count() < count {',
     '            while self.count() <= count {',
     'm10_same_key_concurrent'),
]

LIB_TESTS = {'separate-commit'}

EXPECTED = {
    'cache-key-recent': '#767 M10 recent content is part of the cache key',
    'cache-key-snapshot': '#767 M10 snapshot is part of the cache key',
    'query-normalize': '#767 M10-2 normalized hit',
    'mode-in-key': '#767 result single-routes/4 (retrieve)',
    'ttl-value': '#767 M10-4 ttl boundary miss',
    'ttl-comparison': '#767 M10-4 ttl boundary miss',
    'reset-not-clear': '#767 M10-5 reset clears',
    'cache-never-set': '#767 M10-7 reset during await still caches',
    'hit-elapsed': '#767 M10-2 hit elapsedMs is zero',
    'hit-stats': '#767 M10-2 stats lastRecallCacheHit',
    'utf16-concat': '#767 UTF-16 trace output summary keeps the lone surrogate',
    'separate-commit': '#767 P1-1 cache insert and stats write are one critical section',
    'injected-blocks-user': '#767 result empty-user-profile/0 (retrieve)',
    'same-key-or-insert': '#767 M10-9 reverse later completion wins',
    'gated-wait-bound': '#767 M10-8 same-key concurrent requests: gated transport timed out after 5s',
}

names = [case[0] for case in cases]
assert len(names) == len(set(names)), 'duplicate control name'
all_cases = list(cases)
if sys.argv[1:] == ['--list']:
    print('\n'.join(names))
    raise SystemExit(0)
if sys.argv[1:] == ['--check-anchors']:
    originals = {path: path.read_text() for path in {case[1] for case in cases}}
    stale = []
    for name, path, before, _after, _test in all_cases:
        count = originals[path].count(before)
        if count != 1:
            stale.append((name, count, path.relative_to(root).as_posix()))
    if stale:
        for name, count, relative in stale:
            print(f'STALE ANCHOR {name}: {count} matches in {relative}')
        print('STALE ANCHORS: negative.py is out of sync with its sources; refusing to run')
        raise SystemExit(2)
    print(f'{len(all_cases)} anchors: ok')
    raise SystemExit(0)

assert len(sys.argv) >= 2, 'usage: negative.py OUT_DIR [NAME ...] | negative.py --list | --check-anchors'
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
paths = sorted({case[1] for case in cases})

env = dict(os.environ)
env['CARGO_TARGET_DIR'] = target
base_command = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
                '-p', 'pilotdeck-gateway']
results = []


def test_command(test, case_target):
    if case_target == 'lib':
        return base_command + ['--lib', test, '--', '--exact']
    return base_command + ['--test', 'edgeclaw_retriever', test, '--', '--exact']


def classify(log, returncode, expected):
    if returncode is None:
        return 'INVALID', 'no exit code'
    if 'could not compile' in log:
        return 'INVALID', 'compiler failure'
    if returncode != 0 and 'test result: FAILED.' in log and 'running 1 test' in log:
        if expected in log:
            return 'KILLED', expected
        return 'INVALID', f'failed without the bound assertion {expected}'
    if returncode == 0 and 'test result: ok.' in log:
        return 'SURVIVED', 'test stayed green'
    return 'INVALID', 'unclassified output'


def write_summary(baseline):
    (out / 'summary.json').write_text(json.dumps(
        {'baseline': baseline, 'cases': results}, indent=2) + '\n')


with mutating_sources(root, paths) as originals:
    stale = []
    for name, path, before, _after, _test in cases:
        count = originals[path].decode().count(before)
        if count != 1:
            stale.append((name, path.relative_to(root).as_posix(), count))
    if stale:
        report = '\n'.join(
            f'STALE ANCHOR {name}: {count} matches in {relative}'
            for name, relative, count in stale) + '\n'
        (out / 'stale-anchors.log').write_text(report)
        print(report, end='', flush=True)
        print('STALE ANCHORS: negative.py is out of sync with its sources; refusing to run',
              flush=True)
        raise SystemExit(2)
    baseline_ok = False
    baseline = {'name': 'baseline', 'outcome': 'INVALID', 'returncode': None}
    try:
        try:
            slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
            slots_text = slots.stdout + slots.stderr
            run = run_owned(base_command + ['--test', 'edgeclaw_retriever', '--', '--test-threads=1'],
                            cwd=root, env=env, timeout=600)
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
            case_target = 'lib' if name in LIB_TESTS else 'test'
            command = test_command(test, case_target)
            try:
                slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
                slots_text = slots.stdout + slots.stderr
                original = originals[path].decode()
                path.write_bytes(original.replace(before, after).encode())
                try:
                    run = run_owned(command, cwd=root, env=env, timeout=600)
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
            outcome, reason = classify(log, returncode, EXPECTED[name])
            if outcome == 'KILLED' and not baseline_ok:
                outcome, reason = 'INVALID', 'baseline was not green'
            (out / f'{name}.log').write_text(
                'COMMAND ' + ' '.join(command) + '\n' +
                slots_text + log + f'\nexit={returncode}\nexpected={EXPECTED[name]}\n')
            results.append({'name': name, 'test': test, 'target': case_target,
                            'outcome': outcome, 'reason': reason,
                            'returncode': returncode, 'anchor': before,
                            'replacement': after})
            write_summary(baseline)
            print(f'{name}: {outcome} ({reason}) exit={returncode}', flush=True)
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

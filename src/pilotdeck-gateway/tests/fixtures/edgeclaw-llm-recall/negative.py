#!/usr/bin/env python3
"""Reversible product mutations for #765 retrieval-side LLM calls.

Every case must compile and fail the bound ``#765 result/requests <case>``
assertion in ``replay_matrix``. Anchors are checked against the baseline bytes
before anything runs: a stale anchor is a harness defect, so it prints
``STALE ANCHOR`` and exits 2 (never SURVIVED). A surviving case does not stop
the run; every selected case executes, then the script prints a
KILLED / SURVIVED / INVALID summary and exits 1 when anything is not KILLED.

``recall.rs`` is a new file, so before the coordinator commits it
``mutating_sources`` intentionally refuses to run (it requires source bytes to
match HEAD). Use ``--check-anchors`` pre-commit; run the mutants only after the
commit, on the host:

    CARGO_TARGET_DIR=/abs/target python3 negative.py OUT_DIR
    CARGO_TARGET_DIR=/abs/target python3 negative.py OUT_DIR source-priority
    python3 negative.py --check-anchors
    python3 negative.py --list

Never run this in a shadow repository or alternate source tree.
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
source = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/llm/recall.rs'

cases = [
    # Lower `general_local` below `workspace_external`, so the older local
    # project loses the fallback tie-break.
    # oracle: select-fallback-priority expects LocalOlder.
    ('source-priority', source,
     '''        Some(value)
            if value.equals_str("general_local")
                || value.equals_str("workspace_external_mirror") =>
        {
            2.0
        }
        Some(value) if value.equals_str("workspace_external") => 1.0,''',
     '''        Some(value)
            if value.equals_str("general_local")
                || value.equals_str("workspace_external_mirror") =>
        {
            1.0
        }
        Some(value) if value.equals_str("workspace_external") => 3.0,''',
     'replay_matrix'),
    # Replace `localeCompare` with raw UTF-16 order: "a" then outranks "B".
    # oracle: select-fallback-updated-locale expects UpperB.
    ('fallback-updated', source,
     '                node_compare_utf16(recall_collator(), &right.updated_at, &left.updated_at)',
     '                right.updated_at.units().cmp(left.updated_at.units())',
     'replay_matrix'),
    # Do not trim the model id before matching; a padded id no longer matches.
    # oracle: select-match-whitespace-id expects project p1 without a fallback reason.
    ('selected-id-trim', source,
     '        .map(|value| JsString::from_utf16(js_trim_units(value.units()).to_vec()))',
     '        .map(|value| value.clone())',
     'replay_matrix'),
    # Swap the two allowEmpty default reasons.
    # oracle: select-outside-allow-empty expects the outside-shortlist text.
    ('allow-empty-reason', source,
     '                            JsString::from("Model returned a project id outside the shortlist.")',
     '                            JsString::from("Model returned no matching project.")',
     'replay_matrix'),
    # Send only 199 manifest entries; the 200th recorded prompt entry is lost.
    # oracle: manifest-200 records exactly 200.
    ('manifest-slice', source,
     '                    .take(200)',
     '                    .take(199)',
     'replay_matrix'),
    # Drop the upper Math.min(5, ...) clamp for the manifest limit.
    # oracle: manifest-limit-over asks for limit 5 while the input is 9.
    ('manifest-limit', source,
     '    js_max(1.0, js_min(5.0, limit.unwrap_or(5.0)))',
     '    js_max(1.0, limit.unwrap_or(5.0))',
     'replay_matrix'),
    # Use the last three, not four, recent messages of the decide prompt.
    # oracle: decide-recent-slice drops the fourth retained message.
    ('decide-recent-slice', source,
     '''fn recent_message_pairs(messages: &[MemoryMessage]) -> Vec<JsValue> {
    let start = messages.len().saturating_sub(4);''',
     '''fn recent_message_pairs(messages: &[MemoryMessage]) -> Vec<JsValue> {
    let start = messages.len().saturating_sub(3);''',
     'replay_matrix'),
    # Same off-by-one for the user-message strings used by the other prompts.
    # oracle: select-recent-user-slice drops "message 2".
    ('user-recent-slice', source,
     '''fn recent_user_texts(messages: &[MemoryMessage]) -> Vec<JsValue> {
    let start = messages.len().saturating_sub(4);''',
     '''fn recent_user_texts(messages: &[MemoryMessage]) -> Vec<JsValue> {
    let start = messages.len().saturating_sub(3);''',
     'replay_matrix'),
    # Render a missing sourceType as "none" instead of "unknown".
    # oracle: select-source-unknown prompt source_type.
    ('project-source-unknown', source,
     '            None => JsValue::String("unknown".into()),',
     '            None => JsValue::String("none".into()),',
     'replay_matrix'),
    # Use the id in the fallback reason instead of the project name.
    # oracle: select-outside-fallback expects "Fallback selected Alpha".
    ('fallback-reason-name', source,
     '''                    JsValue::String(reason.unwrap_or_else(|| {
                        concat_reason(
                            "Fallback selected ",
                            &fallback_name,
                            "; model returned no valid project id.",
                        )
                    })),''',
     '''                    JsValue::String(reason.unwrap_or_else(|| {
                        concat_reason(
                            "Fallback selected ",
                            &fallback_id,
                            "; model returned no valid project id.",
                        )
                    })),''',
     'replay_matrix'),
    # Off-by-one on the manifest description bound (200 -> 201).
    # oracle: manifest-description-truncate prompt description.
    ('manifest-description-bound', source,
     '        JsValue::String(truncate_for_prompt(&entry.description, 200.0)),',
     '        JsValue::String(truncate_for_prompt(&entry.description, 201.0)),',
     'replay_matrix'),
    # Off-by-one on the shortlist description bound (180 -> 181).
    # oracle: select-description-truncate prompt description.
    ('shortlist-description-bound', source,
     '        JsValue::String(truncate_for_prompt(&project.description, 180.0)),',
     '        JsValue::String(truncate_for_prompt(&project.description, 181.0)),',
     'replay_matrix'),
    # Off-by-one on the project-selection reason bound (220 -> 221).
    # oracle: select-reason-truncate result length.
    ('reason-bound', source,
     '    Some(truncate_for_prompt(reason, 220.0))',
     '    Some(truncate_for_prompt(reason, 221.0))',
     'replay_matrix'),
    # JSON.stringify(..., null, 2): double the indentation width.
    # oracle: every recorded request body.
    ('pretty-indent', source,
     "    out.extend(std::iter::repeat_n(' ', depth * 2));",
     "    out.extend(std::iter::repeat_n(' ', depth * 4));",
     'replay_matrix'),
]

EXPECTED = {
    'source-priority': '#765 result select-fallback-priority',
    'fallback-updated': '#765 result select-fallback-updated-locale',
    'selected-id-trim': '#765 result select-match-whitespace-id',
    'allow-empty-reason': '#765 result select-outside-allow-empty',
    'manifest-slice': '#765 requests manifest-200',
    'manifest-limit': '#765 result manifest-limit-over',
    'decide-recent-slice': '#765 requests decide-recent-slice',
    'user-recent-slice': '#765 requests select-recent-user-slice',
    'project-source-unknown': '#765 requests select-source-unknown',
    'fallback-reason-name': '#765 result select-outside-fallback',
    'manifest-description-bound': '#765 requests manifest-description-truncate',
    'shortlist-description-bound': '#765 requests select-description-truncate',
    'reason-bound': '#765 result select-reason-truncate',
    'pretty-indent': '#765 requests decide-project',
}

names = [case[0] for case in cases]
assert len(names) == len(set(names)), 'duplicate control name'
all_cases = list(cases)
if sys.argv[1:] == ['--list']:
    print('\n'.join(names))
    raise SystemExit(0)
if sys.argv[1:] == ['--check-anchors']:
    original = source.read_text()
    stale = []
    for name, _path, before, _after, _test in all_cases:
        count = original.count(before)
        if count != 1:
            stale.append((name, count))
    if stale:
        for name, count in stale:
            print(f'STALE ANCHOR {name}: {count} matches in {source.relative_to(root).as_posix()}')
        print('STALE ANCHORS: negative.py is out of sync with recall.rs; refusing to run')
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

env = dict(os.environ)
env['CARGO_TARGET_DIR'] = target
command = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
           '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_llm_recall']
results = []


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


with mutating_sources(root, [source]) as originals:
    original = originals[source].decode()
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
        print('STALE ANCHORS: negative.py is out of sync with recall.rs; refusing to run',
              flush=True)
        raise SystemExit(2)
    baseline_ok = False
    baseline = {'name': 'baseline', 'outcome': 'INVALID', 'returncode': None}
    try:
        try:
            slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
            slots_text = slots.stdout + slots.stderr
            run = run_owned(command + ['replay_matrix', '--', '--exact'],
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
            outcome, reason = classify(log, returncode, EXPECTED[name])
            if outcome == 'KILLED' and not baseline_ok:
                outcome, reason = 'INVALID', 'baseline was not green'
            (out / f'{name}.log').write_text(
                'COMMAND ' + ' '.join(command + [test, '--', '--exact']) + '\n' +
                slots_text + log + f'\nexit={returncode}\nexpected={EXPECTED[name]}\n')
            results.append({'name': name, 'test': test, 'outcome': outcome,
                            'reason': reason, 'returncode': returncode,
                            'anchor': before, 'replacement': after})
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

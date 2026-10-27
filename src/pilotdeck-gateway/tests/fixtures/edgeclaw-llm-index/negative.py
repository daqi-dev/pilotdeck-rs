#!/usr/bin/env python3
"""#768 source mutations. Run only after the new source is committed.

Every anchor must occur exactly once, otherwise exit 2 (never SURVIVED).
CARGO_TARGET_DIR must be provided; no alternate source tree or target is made.
No compiler error counts as KILLED. Each mutant has a restored positive run.
Use --check-anchors without mutation, or OUT_DIR [NAME ...] after commit.

index.rs is a new file, so mutating_sources refuses it until the coordinator
commits; only --check-anchors runs pre-commit.
"""
import os
import re
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/llm/index.rs'
cases = [
    ('dedupe-seen', source, 'if seen.contains(&kind)', 'if false', 'result classify-dedupe'),
    ('should-store-truthy', source, 'parsed.get("should_store").is_some_and(js_truthy)', 'parsed.get("should_store").is_some()', 'result classify-should-store-null'),
    ('window-prev-2', source, 'let start = index.saturating_sub(2);', 'let start = index.saturating_sub(1);', 'requests classify-basic'),
    ('window-next-2', source, 'let end = (index + 3).min(turns.len());', 'let end = (index + 2).min(turns.len());', 'requests classify-basic'),
    ('turn-index-one-based', source, '("turn_index", JsValue::Number((index + 1) as f64)),', '("turn_index", JsValue::Number(index as f64)),', 'requests classify-basic'),
    ('agent-empty-gate', source, 'agent_id.clone().filter(|agent_id| !agent_id.is_empty())', 'agent_id.clone()', 'requests classify-agent-empty'),
    ('create-raw-response', source, 'let mut raw_response = JsString::default();', 'let mut raw_response = "mutant".into();', 'debug create-http-400'),
    ('skip-strict-bool', source, 'matches!(parsed.get("skip"), Some(JsValue::Bool(true)))', 'parsed.get("skip").is_some()', 'result create-skip-string'),
    ('generic-project-name', source, 'if kind == MemoryCreateKind::Project && is_generic_project_candidate_name(&name) {', 'if false {', 'result create-generic-project-name'),
    ('selected-id-trim', source, 'JsString::from_utf16(js_trim_units(selected.units()).to_vec())', 'selected.clone()', 'result select-attach-id-trim'),
    ('invalid-id-default-reason', source, '"Model selected an invalid project id."', '"Model selected an invalid project."', 'result select-attach-invalid-id-no-reason'),
    ('recent-last-4', source, 'let recent_start = recent.len().saturating_sub(4);', 'let recent_start = recent.len().saturating_sub(3);', 'requests select-attach'),
    ('list-slice-10', source, '.take(10)', '.take(9)', 'requests select-attach'),
    ('error-key-name', source, '("errorMessage", JsValue::String(error.message())),', '("error", JsValue::String(error.message())),', 'debug create-no-fallback'),
]

# A mutant is killed only by its named assertion in its exact test's panic block.
messages = {
    'dedupe-seen': 'result classify-dedupe',
    'should-store-truthy': 'result classify-should-store-null',
    'window-prev-2': 'requests classify-basic',
    'window-next-2': 'requests classify-basic',
    'turn-index-one-based': 'requests classify-basic',
    'agent-empty-gate': 'requests classify-agent-empty',
    'create-raw-response': 'debug create-http-400',
    'skip-strict-bool': 'result create-skip-string',
    'generic-project-name': 'result create-generic-project-name',
    'selected-id-trim': 'result select-attach-id-trim',
    'invalid-id-default-reason': 'result select-attach-invalid-id-no-reason',
    'recent-last-4': 'requests select-attach',
    'list-slice-10': 'requests select-attach',
    'error-key-name': 'debug create-no-fallback',
}

def classify(completed, test, message):
    log = completed.stdout + completed.stderr
    if 'could not compile' in log or not re.search(r'^running 1 test$', log, re.M):
        return 'INVALID'
    if completed.returncode == 0:
        return 'PASS' if (re.search(r'^test ' + re.escape(test) + r' \.\.\. ok$', log, re.M)
                          and 'test result: ok. 1 passed; 0 failed;' in log) else 'INVALID'
    if completed.returncode != 101 or not re.search(r'^test ' + re.escape(test) + r' \.\.\. FAILED$', log, re.M):
        return 'INVALID'
    if 'test result: FAILED. 0 passed; 1 failed;' not in log:
        return 'INVALID'
    blocks = re.findall(r'^---- (.+) stdout ----\n(.*?)(?=^---- |^failures:|\Z)', log, re.M | re.S)
    if len(blocks) != 1 or blocks[0][0] != test:
        return 'INVALID'
    panic = blocks[0][1]
    threads = re.findall(r"^thread '([^']+)'(?: \(\d+\))? panicked at .+:\d+:\d+:$", panic, re.M)
    if threads != [test]:
        return 'INVALID'
    # Match the assertion line, never an expected string embedded in left/right data.
    return 'KILLED' if re.search(r'^(?:assertion .* failed: )?' + re.escape(message) + r'$', panic, re.M) else 'INVALID'

def anchors(text):
    stale = []
    for name, path, before, _, _ in cases:
        contents = text if path == source else path.read_text()
        if contents.count(before) != 1:
            stale.append((name, contents.count(before)))
    assert set(messages) == {case[0] for case in cases}, 'every mutant needs its own named assertion'
    if stale:
        for name, count in stale: print(f'STALE ANCHOR {name}: {count} matches', file=sys.stderr)
        raise SystemExit(2)

anchors(source.read_text())
if sys.argv[1:] == ['--check-anchors']:
    print(f'{len(cases)} unique anchors; no mutation')
    raise SystemExit(0)
assert os.environ.get('CARGO_TARGET_DIR'), 'CARGO_TARGET_DIR must be supplied by caller'
assert len(sys.argv) >= 2, 'usage: negative.py OUT_DIR [NAME ...] | --check-anchors'
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
selected = set(sys.argv[2:])
assert selected <= {c[0] for c in cases}, 'unknown case'
command = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'), '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_llm_index']

def run(name, test, message):
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    completed = run_owned(command + [test, '--', '--exact'], env=dict(os.environ), cwd=root, timeout=600)
    log = slots.stdout + slots.stderr + completed.stdout + completed.stderr
    with (out / f'{name}.log').open('x') as f: f.write(log)
    return classify(completed, test, message)

failed = []
# This guard intentionally rejects a new uncommitted source file. Do not bypass it.
with mutating_sources(root, [source]) as originals:
    original = originals[source].decode()
    anchors(original)
    for name, path, before, after, message in cases:
        if selected and name not in selected: continue
        assert message == messages[name], 'case message must match its named assertion'
        assert run(name + '-before', 'oracle_matrix', message) == 'PASS', 'positive baseline failed'
        try:
            path.write_text(original.replace(before, after))
            outcome = run(name, 'oracle_matrix', message)
        finally:
            path.write_bytes(originals[path])
        restored = run(name + '-restored', 'oracle_matrix', message)
        print(f'{name}: {"SURVIVED" if outcome == "PASS" else outcome}; restored={restored}', flush=True)
        if outcome != 'KILLED' or restored != 'PASS': failed.append(name)
raise SystemExit(1 if failed else 0)

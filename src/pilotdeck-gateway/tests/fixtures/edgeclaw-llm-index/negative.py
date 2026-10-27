#!/usr/bin/env python3
"""#768 source mutations. Run only after the new source is committed.

Every anchor must occur exactly once, otherwise exit 2 (never SURVIVED).
CARGO_TARGET_DIR must be provided; no alternate source tree or target is made.
No compiler error counts as KILLED. Each mutant has a restored positive run.

A per-mutant timeout or subprocess failure is recorded as TIMEOUT/INVALID
(with the captured output in <name>.log) and the run continues to the next
mutant; only a source-restore failure aborts immediately, because a wrong
source tree would poison every later verdict. The final summary exits
non-zero when any mutant is not KILLED.

index.rs is a new file, so mutating_sources refuses it until the coordinator
commits; only --check-anchors runs pre-commit.
"""
import os
import re
import subprocess
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/llm/index.rs'
cases = [
    ('dedupe-seen', source, 'if seen.contains(&kind)', 'if false', 'result classify-dedupe'),
    ('should-store-truthy', source, 'parsed.get("should_store").is_some_and(js_truthy)', 'parsed.get("should_store").is_some()', 'result classify-should-store-null'),
    ('window-prev-2', source, 'let start = index.saturating_sub(2);', 'let start = index.saturating_sub(1);', 'requests classify-window-last'),
    ('window-next-2', source, 'let end = (index + 3).min(turns.len());', 'let end = (index + 2).min(turns.len());', 'requests classify-basic'),
    ('turn-index-one-based', source, '("turn_index", JsValue::Number((index + 1) as f64)),', '("turn_index", JsValue::Number(index as f64)),', 'requests classify-basic'),
    # The old agent-empty-gate mutant (dropping truthy_agent_id's empty filter)
    # is equivalent: resolve_agent_primary_model re-filters empty ids, as TS
    # gates twice too. This gate of the same TS truthiness spread is observable.
    ('session-key-gate', source, 'if let Some(session_key) = session_key.filter(|session_key| !session_key.is_empty())', 'if let Some(session_key) = session_key.filter(|_| false)', 'result create-user-basic'),
    ('create-raw-response', source, 'let mut raw_response = JsString::default();', 'let mut raw_response = "mutant".into();', 'debug create-http-400'),
    ('skip-strict-bool', source, 'matches!(parsed.get("skip"), Some(JsValue::Bool(true)))', 'parsed.get("skip").is_some()', 'result create-user-basic'),
    ('generic-project-name', source, 'if kind == MemoryCreateKind::Project && is_generic_project_candidate_name(&name) {', 'if false {', 'result create-generic-project-name'),
    ('selected-id-trim', source, 'JsString::from_utf16(js_trim_units(selected.units()).to_vec())', 'selected.clone()', 'result select-attach-id-trim'),
    ('invalid-id-default-reason', source, '"Model selected an invalid project id."', '"Model selected an invalid project."', 'result select-attach-invalid-id-no-reason'),
    ('recent-last-4', source, 'let recent_start = recent.len().saturating_sub(4);', 'let recent_start = recent.len().saturating_sub(3);', 'requests select-attach'),
    ('list-slice-10', source, '.take(10)', '.take(9)', 'requests select-attach'),
    ('error-key-name', source, '("errorMessage", JsValue::String(error.message())),', '("error", JsValue::String(error.message())),', 'debug create-no-fallback'),
    # -- fix-r2: write_pretty depth bound, surrogate-preserving parse, and the
    # fallback warnings. Bindings are the first assertion each mutation moves,
    # in oracle order (classify before create before select; within a case
    # result before requests before debug before warnings).
    ('pretty-depth-gate', source, 'if frames.len() >= MAX_ARRAY_DEPTH {', 'if frames.len() >= usize::MAX {', 'result classify-deep-meta'),
    ('pretty-range-message', source, 'Self::Range => ("RangeError", JsString::from(StackOverflow::MESSAGE)),', 'Self::Range => ("Rangeerror", JsString::from(StackOverflow::MESSAGE)),', 'warnings classify-deep-meta'),
    ('pretty-separator', source, 'Some(item) => {\n                    if *written {\n                        out.push_str(",\\n");', 'Some(item) => {\n                    if *written {\n                        out.push_str(",");', 'requests classify-basic'),
    ('strict-parse-lossy', source, 'let decoded = text::json_text(envelope.units());', 'let decoded = String::from_utf16_lossy(envelope.units());', 'result classify-lone-surrogate-reason'),
    ('replace-error-text', source, '"value.replace is not a function"', '"value.replace is not a method"', 'warnings classify-meta-description-number'),
    ('classify-warning-prefix', source, '"[clawxmemory] memory turn classification fallback: "', '"[clawxmemory] memory turn classification fallback! "', 'warnings classify-meta-missing-description'),
    ('create-warning-kind', source, 'MemoryCreateKind::User => "user memory create",', 'MemoryCreateKind::User => "user memory creation",', 'warnings create-no-fallback'),
    ('create-warning-kind-project', source, 'MemoryCreateKind::Project => "project memory create",', 'MemoryCreateKind::Project => "project memory creation",', 'warnings create-project-no-model'),
    ('create-warning-kind-feedback', source, 'MemoryCreateKind::Feedback => "feedback memory create",', 'MemoryCreateKind::Feedback => "feedback memory creation",', 'warnings create-feedback-no-model'),
    ('select-warning-prefix', source, '"[clawxmemory] file memory project assignment fallback: "', '"[clawxmemory] file memory project assignment fallback! "', 'warnings select-shortlist-missing-description'),
    # truncated_list's V8 source-expression messages (the audit catch of
    # fix-r2: the receiver expression, never the value, is the message).
    ('truncated-list-slice-message', source, '(input.candidate.{field} ?? []).slice is not a function', '(input.candidate.{field} ?? []).slice is not a method', 'warnings select-candidate-decisions-number'),
    ('truncated-list-map-message', source, '(input.candidate.{field} ?? []).slice(...).map is not a function', '(input.candidate.{field} ?? []).slice(...).map is not a method', 'warnings select-candidate-constraints-string'),
]

# A mutant is killed only by its named assertion in its exact test's panic block.
messages = {
    'dedupe-seen': 'result classify-dedupe',
    'should-store-truthy': 'result classify-should-store-null',
    'window-prev-2': 'requests classify-window-last',
    'window-next-2': 'requests classify-basic',
    'turn-index-one-based': 'requests classify-basic',
    'session-key-gate': 'result create-user-basic',
    'create-raw-response': 'debug create-http-400',
    'skip-strict-bool': 'result create-user-basic',
    'generic-project-name': 'result create-generic-project-name',
    'selected-id-trim': 'result select-attach-id-trim',
    'invalid-id-default-reason': 'result select-attach-invalid-id-no-reason',
    'recent-last-4': 'requests select-attach',
    'list-slice-10': 'requests select-attach',
    'error-key-name': 'debug create-no-fallback',
    'pretty-depth-gate': 'result classify-deep-meta',
    'pretty-range-message': 'warnings classify-deep-meta',
    'pretty-separator': 'requests classify-basic',
    'strict-parse-lossy': 'result classify-lone-surrogate-reason',
    'replace-error-text': 'warnings classify-meta-description-number',
    'classify-warning-prefix': 'warnings classify-meta-missing-description',
    'create-warning-kind': 'warnings create-no-fallback',
    'create-warning-kind-project': 'warnings create-project-no-model',
    'create-warning-kind-feedback': 'warnings create-feedback-no-model',
    'select-warning-prefix': 'warnings select-shortlist-missing-description',
    'truncated-list-slice-message': 'warnings select-candidate-decisions-number',
    'truncated-list-map-message': 'warnings select-candidate-constraints-string',
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
    """One named test run. A subprocess timeout or spawn failure is a
    recorded status plus a saved log, never an abort of the whole sweep."""
    prefix = ''
    try:
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
        prefix = slots.stdout + slots.stderr
        completed = run_owned(command + [test, '--', '--exact'], env=dict(os.environ), cwd=root, timeout=600)
    except subprocess.TimeoutExpired as expired:
        # run_owned already killed the process group; keep whatever output
        # the timeout captured so the log still says why it hung.
        detail = f'TIMEOUT after {expired.timeout}s'
        for stream in (expired.stdout, expired.stderr):
            if stream:
                detail += '\n' + (stream.decode(errors='replace') if isinstance(stream, bytes) else str(stream))
        with (out / f'{name}.log').open('x') as f: f.write(prefix + detail)
        return 'TIMEOUT'
    except (subprocess.SubprocessError, OSError) as error:
        with (out / f'{name}.log').open('x') as f: f.write(prefix + f'INVALID: {error!r}')
        return 'INVALID'
    log = prefix + completed.stdout + completed.stderr
    with (out / f'{name}.log').open('x') as f: f.write(log)
    return classify(completed, test, message)

verdicts = {}
failed = []
# This guard intentionally rejects a new uncommitted source file. Do not bypass it.
with mutating_sources(root, [source]) as originals:
    original = originals[source].decode()
    anchors(original)
    for name, path, before, after, message in cases:
        if selected and name not in selected: continue
        assert message == messages[name], 'case message must match its named assertion'
        baseline = run(name + '-before', 'oracle_matrix', message)
        if baseline != 'PASS':
            # The unmutated source already fails, so this mutant proves
            # nothing; record it and keep sweeping the remaining mutants.
            verdicts[name] = f'INVALID: baseline {baseline}'
            failed.append(name)
            print(f'{name}: {verdicts[name]}', flush=True)
            continue
        try:
            path.write_text(original.replace(before, after))
            outcome = run(name, 'oracle_matrix', message)
        finally:
            # A restore failure aborts immediately: every later verdict would
            # be measured against a poisoned source.
            path.write_bytes(originals[path])
        restored = run(name + '-restored', 'oracle_matrix', message)
        verdicts[name] = f'{"SURVIVED" if outcome == "PASS" else outcome}; restored={restored}'
        print(f'{name}: {verdicts[name]}', flush=True)
        if outcome != 'KILLED' or restored != 'PASS': failed.append(name)
killed = sum(1 for verdict in verdicts.values() if verdict.startswith('KILLED'))
print(f'--- {len(verdicts)} mutants: KILLED={killed} '
      f'non-KILLED={len(verdicts) - killed}')
for name, verdict in verdicts.items():
    if not verdict.startswith('KILLED'):
        print(f'non-KILLED {name}: {verdict}', file=sys.stderr)
raise SystemExit(1 if failed else 0)

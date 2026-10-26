#!/usr/bin/env python3
"""Reversible product mutations for edgeclaw-retrieval-helpers.

Every case must compile and fail a runtime oracle assertion; the script restores
the original bytes in `finally` and never touches oracle.json.
"""
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
helpers = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/retrieval/helpers.rs'
out = Path(sys.argv[1]).resolve()
target = os.environ.get('CARGO_TARGET_DIR')
assert target, 'negative.py requires CARGO_TARGET_DIR from the caller'
out.mkdir(parents=True, exist_ok=True)
cases = [
    # Han detection drives the 2/3-unit slicing; Latin instead of Han leaves
    # every CJK token whole.
    ('han-script-split', helpers,
     'regex::Regex::new(r"\\p{Script=Han}")',
     'regex::Regex::new(r"\\p{Script=Latin}")',
     'cjk_tokenization_matches_ts'),
    # localeCompare ordering is not code-unit ordering: "a" sorts before "B".
    ('locale-compare', helpers,
     '''            .then_with(|| {
                node_compare_utf16(
                    collator,
                    &string_field(right, "updatedAt"),
                    &string_field(left, "updatedAt"),
                )
            })''',
     '''            .then_with(|| {
                string_field(right, "updatedAt")
                    .units()
                    .cmp(string_field(left, "updatedAt").units())
            })''',
     'project_shortlist_matches_ts'),
    # Keep leading whitespace: normalizeQueryKey no longer trims.
    ('normalize-trim', helpers,
     'if pending_space && !units.is_empty() {',
     'if pending_space {',
     'normalize_and_trace_id_match_ts'),
    # Project metadata loses its `|| "No project description yet."` fallback.
    ('project-description-fallback', helpers,
     '_ => JsString::from("No project description yet."),',
     '_ => JsString::default(),',
     'render_context_matches_ts'),
    # Stop removing records already covered by the user summary.
    ('context-user-dedupe', helpers,
     '.filter(|record| !user_paths.contains(&string_field(record, "relativePath")))',
     '.filter(|_record| true)',
     'render_context_matches_ts'),
    # Every trace id drifts by one millisecond of injected entropy.
    ('trace-clock-entropy', helpers,
     'seed_units.extend(now_ms.to_string().encode_utf16());',
     'seed_units.extend((now_ms + 1).to_string().encode_utf16());',
     'normalize_and_trace_id_match_ts'),
    # The user fallback keeps only one id; project/mix must keep three.
    ('fallback-limit', helpers,
     'let limit = if route.equals_str("user") { 1 } else { 3 };',
     'let limit = if route.equals_str("user") { 1 } else { 2 };',
     'selection_and_empty_result_match_ts'),
    # Recent-user selection drops one message from the four-message window.
    ('recent-window', helpers,
     'users[users.len().saturating_sub(4)..].to_vec()',
     'users[users.len().saturating_sub(3)..].to_vec()',
     'selection_and_empty_result_match_ts'),
    # Shortlist truncation must be exactly 30.
    ('shortlist-limit', helpers,
     'candidates.truncate(GENERAL_RECALL_PROJECT_CANDIDATE_LIMIT);',
     'candidates.truncate(GENERAL_RECALL_PROJECT_CANDIDATE_LIMIT - 1);',
     'project_shortlist_matches_ts'),
    # Step numbering is one-based and derived from the current length.
    ('step-number', helpers,
     'step_id.extend((steps.len() + 1).to_string().encode_utf16());',
     'step_id.extend(steps.len().to_string().encode_utf16());',
     'trace_and_steps_match_ts'),
    # hasUserSummary is a length check, not an always-true identity.
    ('user-summary-truth', helpers,
     '!identity_background(user_summary).is_empty()',
     'identity_background(user_summary).is_empty()',
     'render_context_matches_ts'),
]
env = dict(os.environ)
env['CARGO_TARGET_DIR'] = target
with mutating_sources(root, sorted({path for _, path, _, _, _ in cases})) as originals:
    try:
        for name, path, before, after, test in cases:
            source = originals[path].decode()
            assert source.count(before) == 1, (name, source.count(before))
            slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
            path.write_text(source.replace(before, after))
            try:
                cmd = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
                       '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_retrieval_helpers',
                       test, '--', '--exact']
                run = run_owned(cmd, cwd=root, env=env, timeout=300)
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

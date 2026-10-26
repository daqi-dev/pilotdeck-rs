#!/usr/bin/env python3
"""Reversible product mutations for edgeclaw-retrieval-helpers.

Every case must compile and fail a narrowed runtime oracle assertion; the
script restores the original bytes in ``finally`` and never touches
``oracle.json``. Every anchor is checked against the baseline bytes before the
unmutated suite runs: a stale anchor is a harness defect, not a surviving
mutant, so it prints ``STALE ANCHOR`` and exits 2 (same rule as #772 fix-r2).

A failed cell does not abort the matrix. Like ``edgeclaw-messages/negative.py``
the script first runs the unmutated suite once and requires exit code 0 (so an
environment panic cannot be mistaken for a kill), then executes every case,
prints KILLED/SURVIVED per cell, and exits 1 when anything survived. A kill
must contain the ``assert_eq!`` failure line and the function-level replay
anchor recorded for that case (for example ``tokenizeSearchText args=``);
``could not compile`` is never a kill. After restoring every byte, the same
unmutated suite runs once more and must pass (``restored.log``).

``negative_process.mutating_sources`` demands that the gateway product tree
match the baseline (HEAD by default); the coordinator can therefore run the
whole matrix after this fixture change is committed. ``NEGATIVE_ONLY=a,b``
runs just those cells for a one-cell sample (the unmutated baseline still runs
first).
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
baseline = os.environ.get('NEGATIVE_BASELINE', 'HEAD')
out.mkdir(parents=True, exist_ok=True)
cases = [
    # Han detection drives the 2/3-unit slicing; Latin instead of Han leaves
    # every CJK token whole.
    ('han-script-split', helpers,
     'regex::Regex::new(r"\\p{Script=Han}")',
     'regex::Regex::new(r"\\p{Script=Latin}")',
     'cjk_tokenization_matches_ts', 'expandCjkSearchToken args='),
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
     'project_shortlist_matches_ts', 'buildProjectShortlist args='),
    # Keep leading whitespace: normalizeQueryKey no longer trims.
    ('normalize-trim', helpers,
     'if pending_space && !units.is_empty() {',
     'if pending_space {',
     'normalize_and_trace_id_match_ts', 'normalizeQueryKey args='),
    # Project metadata loses its `|| "No project description yet."` fallback.
    ('project-description-fallback', helpers,
     '_ => JsString::from("No project description yet."),',
     '_ => JsString::default(),',
     'render_context_matches_ts', 'renderProjectMetaBlock args='),
    # Stop removing records already covered by the user summary.
    ('context-user-dedupe', helpers,
     '.filter(|record| !user_paths.contains(&string_field(record, "relativePath")))',
     '.filter(|_record| true)',
     'render_context_matches_ts', 'renderContext args='),
    # Every trace id drifts by one millisecond of injected entropy.
    ('trace-clock-entropy', helpers,
     'seed_units.extend(now_ms.to_string().encode_utf16());',
     'seed_units.extend((now_ms + 1).to_string().encode_utf16());',
     'normalize_and_trace_id_match_ts', 'buildTraceId args='),
    # The user fallback keeps only one id; project/mix must keep three.
    ('fallback-limit', helpers,
     'let limit = if route.equals_str("user") { 1 } else { 3 };',
     'let limit = if route.equals_str("user") { 1 } else { 2 };',
     'selection_and_empty_result_match_ts', 'fallbackSelection args='),
    # Recent-user selection drops one message from the four-message window.
    ('recent-window', helpers,
     'users[users.len().saturating_sub(4)..].to_vec()',
     'users[users.len().saturating_sub(3)..].to_vec()',
     'selection_and_empty_result_match_ts', 'recentUserMessages args='),
    # Shortlist truncation must be exactly 30.
    ('shortlist-limit', helpers,
     'candidates.truncate(GENERAL_RECALL_PROJECT_CANDIDATE_LIMIT);',
     'candidates.truncate(GENERAL_RECALL_PROJECT_CANDIDATE_LIMIT - 1);',
     'project_shortlist_matches_ts', 'buildProjectShortlist args='),
    # Step numbering is one-based and derived from the current length.
    ('step-number', helpers,
     'step_id.extend((steps.len() + 1).to_string().encode_utf16());',
     'step_id.extend(steps.len().to_string().encode_utf16());',
     'trace_and_steps_match_ts', 'traceScenario args='),
    # hasUserSummary is a length check, not an always-true identity.
    ('user-summary-truth', helpers,
     '!identity_background(user_summary).is_empty()',
     'identity_background(user_summary).is_empty()',
     'render_context_matches_ts', 'renderUserSummaryBlock args='),
    # A CJK stopword disappears from the list, so the oracle's `当前项目`
    # tokenization keeps the bare `当前` 2-gram it expects to be dropped.
    ('stopword-cjk-current', helpers,
     '"项目", "当前", "这个"', '"项目", "这个"',
     'cjk_tokenization_matches_ts', 'tokenizeSearchText args=["当前项目"]'),
    # The same list carries the ASCII stopword `general`; without it the
    # oracle's `GENERAL general GENERAL` tokenization stops being empty.
    ('stopword-general', helpers,
     '"默认", "应该", "需要", "general",',
     '"默认", "应该", "需要",',
     'cjk_tokenization_matches_ts', 'tokenizeSearchText args=["GENERAL general GENERAL"]'),
    # An exact project-name match is weighted 10: the oracle's 2 exact + 1
    # token match must score 21, not 19.
    ('shortlist-exact-weight', helpers,
     'let score = exact * 10 + matched.len() as i64;',
     'let score = exact * 9 + matched.len() as i64;',
     'project_shortlist_matches_ts',
     'buildProjectShortlist args=[[{"projectId":"p1","projectName":"Alpha"'),
    # The render whitelist is exactly user/project/mix; also accepting `auto`
    # renders a context where the oracle expects the empty string.
    ('render-route-whitelist-auto', helpers,
     '} else if route.equals_str("mix") {',
     '} else if route.equals_str("mix") || route.equals_str("auto") {',
     'render_context_matches_ts', 'renderContext args=["auto",'),
    # Unknown routes must fall through to the empty string; rendering the user
    # block instead makes the oracle's `none` route non-empty.
    ('render-route-whitelist-fallthrough', helpers,
     '    } else {\n        return JsString::default();\n    }',
     '    } else {\n        lines.extend(render_user_summary_block(user_summary));\n    }',
     'render_context_matches_ts', 'renderContext args=["none",'),
]
env = dict(os.environ)
env['CARGO_TARGET_DIR'] = target
only = {name for name in os.environ.get('NEGATIVE_ONLY', '').split(',') if name}
unknown = only - {case[0] for case in cases}
assert not unknown, f'unknown NEGATIVE_ONLY cases: {sorted(unknown)}'


def assertion_failure(log, anchor):
    if 'could not compile' in log:
        return False
    return 'assertion `left == right` failed' in log and anchor in log


killed, survived = [], []
with mutating_sources(root, sorted({path for _, path, _, _, _, _ in cases}),
                      baseline=baseline) as originals:
    # A product line that this list did not follow used to be reported as
    # SURVIVED (`AssertionError: (name, 0)`), hiding a broken control behind
    # the same label as a real surviving mutant. Refuse before compiling.
    sources = {path: data.decode() for path, data in originals.items()}
    stale = []
    for name, path, before, _after, _test, _anchor in cases:
        count = sources[path].count(before)
        if count != 1:
            stale.append((name, path.relative_to(root).as_posix(), count))
    if stale:
        report = '\n'.join(
            f'STALE ANCHOR {name}: {count} matches in {relative}'
            for name, relative, count in stale) + '\n'
        (out / 'stale-anchors.log').write_text(report)
        print(report, end='', flush=True)
        print('STALE ANCHORS: negative.py is out of sync with helpers.rs; refusing to run',
              flush=True)
        sys.exit(2)
    base_cmd = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
                '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_retrieval_helpers']
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    run = run_owned(base_cmd, cwd=root, env=env, timeout=600)
    (out / 'baseline.log').write_text(slots.stdout + slots.stderr + run.stdout + run.stderr)
    if run.returncode != 0:
        print('BASELINE FAILED: unmutated suite must pass before mutations', flush=True)
        sys.exit(1)

    for name, path, before, after, test, anchor in cases:
        if only and name not in only:
            continue
        reason = None
        try:
            source = originals[path].decode()
            slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
            path.write_text(source.replace(before, after))
            try:
                cmd = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
                       '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_retrieval_helpers',
                       test, '--', '--exact']
                run = run_owned(cmd, cwd=root, env=env, timeout=300)
                log = run.stdout + run.stderr
                (out / f'{name}.log').write_text(slots.stdout + slots.stderr + log)
                if not (run.returncode != 0 and assertion_failure(log, anchor)):
                    reason = 'did not fail a narrowed runtime assertion'
            finally:
                path.write_bytes(originals[path])
        except Exception as error:
            reason = f'{type(error).__name__}: {error}'
        if reason is None:
            killed.append(name)
            print(f'KILLED   {name}: {test} [{anchor}]', flush=True)
        else:
            survived.append((name, reason))
            print(f'SURVIVED {name}: {reason}', flush=True)
    for path, data in originals.items():
        path.write_bytes(data)
        assert path.read_bytes() == data, 'restoration failed: ' + str(path)
    # The restored bytes must pass the same unmutated suite that gated the run.
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    run = run_owned(base_cmd, cwd=root, env=env, timeout=600)
    (out / 'restored.log').write_text(slots.stdout + slots.stderr + run.stdout + run.stderr)
    if run.returncode != 0:
        print('RESTORED FAILED: unmutated suite must pass after restoration', flush=True)
        sys.exit(1)
print(f'KILLED   {len(killed)}: ' + ', '.join(killed))
print(f'SURVIVED {len(survived)}: ' + ', '.join(name for name, _ in survived))
if survived:
    sys.exit(1)

#!/usr/bin/env python3
"""Reversible product mutations for edgeclaw-dream.

Before any mutation the script runs the unmutated suite once and requires
exit code 0: a panic caused by the environment (for example a bad
``PDRS_BASELINE``) must not be mistaken for a killed mutant. A killed mutant
must then fail on an assertion, not on any panic:

* ``assertion `left == right` failed`` from the replay's ``assert_eq!``, or
* an assertion message beginning with one of the oracle's ``{group}/`` anchors.

``could not compile`` is never a kill. Every case compiles and the original
bytes are restored in ``finally``. ``mutating_sources`` demands that the gateway
product tree match the baseline, so an uncommitted authoring tree is refused;
the coordinator runs this once after committing. For the uncommitted tree,
``NEGATIVE_BASELINE=$(git stash create)`` names a dangling commit of the
current worktree (stage new files first) and ``NEGATIVE_ONLY=a,b`` runs a
sample. The default baseline remains HEAD.

A surviving mutant does not abort the run: every case executes, then the script
prints KILLED/SURVIVED and exits 1 when anything survived. It never starts a
second build directory and cargo always uses the caller's ``CARGO_TARGET_DIR``.
"""
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
fixtures = Path(__file__).resolve().parent
helpers = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/dream/helpers.rs'
js_value = root / 'src/pilotdeck-gateway/src/js_value.rs'
out = Path(sys.argv[1]).resolve()
target = os.environ.get('CARGO_TARGET_DIR')
assert target, 'negative.py requires CARGO_TARGET_DIR from the caller'
baseline = os.environ.get('NEGATIVE_BASELINE', 'HEAD')
out.mkdir(parents=True, exist_ok=True)

# oracle.json has no group field; the replay group is the test name's prefix.
assertion_anchors = ['trace', 'steps', 'mutation', 'sort', 'inputs', 'clusters',
                     'window', 'merge-groups']

overlap_block = '''        if unique_members.iter().any(|member| used.contains(member)) {
            dropped_warnings.push(warning(format!(
                "Dropped overlapping cluster: {}",
                lossy(&join_units(&unique_members, ", "))
            )));
            continue;
        }'''
reused_block = '''        if let Some(reused) = all_ids.iter().find(|id| used_project_ids.contains(*id)) {
            dropped_warnings.push(warning(format!(
                "Dropped {label}: project id {} was already used in another merge group.",
                lossy(reused)
            )));
            continue;
        }'''
keeper_duplicate_block = '''        if duplicates.contains(&keeper) {
            dropped_warnings.push(warning(format!(
                "Dropped {label}: keeper project id was also listed as a duplicate."
            )));
            continue;
        }'''

cases = [
    # 1. Mutation ids ignore Math.random: same action/path/time collapses to one id.
    ('mutation-random', helpers,
     '    seed.extend(random.encode_utf16());',
     '    seed.extend("0".encode_utf16());', 'mutation_matches_ts'),
    # 2. Trace ids ignore the frozen random suffix.
    ('trace-random', helpers,
     '\n    seed.extend(random_suffix.units());', '', 'trace_matches_ts'),
    # 3. Step ids start at 0 instead of steps.length + 1.
    ('step-number', helpers, 'steps.len() + 1', 'steps.len()', 'steps_match_ts'),
    # 4. Refined bodies keep leading/trailing markdown whitespace.
    ('refined-trim', helpers,
     'let mut body = js_trim_units(markdown.units()).to_vec();',
     'let mut body = markdown.units().to_vec();', 'inputs_match_ts'),
    # 5. capturedAt/sourceSessionKey use presence instead of truthiness.
    ('captured-at-truthiness', helpers,
     '''    for key in ["capturedAt", "sourceSessionKey"] {
        put_truthy(&mut out, key, record.get(key));
    }''',
     '''    for key in ["capturedAt", "sourceSessionKey"] {
        put(&mut out, key, record.get(key));
    }''', 'inputs_match_ts'),
    # 6. Same-updatedAt records sort by the tie key descending.
    ('sort-tie-direction', helpers,
     '''                node_compare_utf16(
                    &collator,
                    &js_string_key(left, tie_key),
                    &js_string_key(right, tie_key),
                )''',
     '''                node_compare_utf16(
                    &collator,
                    &js_string_key(right, tie_key),
                    &js_string_key(left, tie_key),
                )''', 'sort_matches_ts'),
    # 7. A cluster exactly at the file limit is rejected.
    ('clusters-max', helpers,
     'if unique_members.len() as f64 > max_files {',
     'if unique_members.len() as f64 >= max_files {', 'clusters_match_ts'),
    # 8. A cluster may reuse files already consumed by an accepted cluster.
    ('clusters-overlap', helpers, overlap_block, '', 'clusters_match_ts'),
    # 9. A reason with both same-project and concrete overlap wording drops.
    ('clusters-generic-and', helpers,
     'same_project_pattern().is_match(&text) && !semantic_pattern().is_match(&text)',
     'same_project_pattern().is_match(&text) || !semantic_pattern().is_match(&text)',
     'clusters_match_ts'),
    # 10. Unknown paths are no longer filtered before counting a cluster.
    ('clusters-allowed', helpers,
     '&& allowed_relative_paths.contains(&member)',
     '&& (allowed_relative_paths.contains(&member) || true)', 'clusters_match_ts'),
    # 11. A record that exactly fills the 120k budget is rejected.
    ('window-budget-geq', helpers,
     'if next_chars > DREAM_USER_NOTE_CHAR_BUDGET {',
     'if next_chars >= DREAM_USER_NOTE_CHAR_BUDGET {', 'window_matches_ts'),
    # 12. An oversized record is skipped instead of stopping the window.
    ('window-break-continue', helpers,
     '''        if next_chars > DREAM_USER_NOTE_CHAR_BUDGET {
            break;
        }''',
     '''        if next_chars > DREAM_USER_NOTE_CHAR_BUDGET {
            continue;
        }''', 'window_matches_ts'),
    # 13. The 200-file cap becomes 199.
    ('window-max-files', helpers,
     'const DREAM_USER_NOTE_MAX_FILES: usize = 200;',
     'const DREAM_USER_NOTE_MAX_FILES: usize = 199;', 'window_matches_ts'),
    # 14. A project id may appear in two merge groups.
    ('merge-reused', helpers, reused_block, '', 'merge_groups_match_ts'),
    # 15. The keeper may also be listed as its own duplicate.
    ('merge-keeper-duplicate', helpers, keeper_duplicate_block, '', 'merge_groups_match_ts'),
    # 16. The empty keeper placeholder disappears from the warning.
    ('merge-empty-label', helpers,
     '''                if keeper.is_empty() {
                    "(empty)".into()
                } else {
                    lossy(&keeper)
                }''',
     '                lossy(&keeper)', 'merge_groups_match_ts'),
    # 17. A reason's U+017F / U+212A fold onto ASCII s / k again.
    ('clusters-ascii-fold', helpers,
     '    let text = ascii_fold_haystack(&text);\n',
     '', 'clusters_match_ts'),
    # 18. The 200-file cap becomes exclusive, so the 201st record is accepted.
    ('window-count-geq', helpers,
     '        if selected.len() >= DREAM_USER_NOTE_MAX_FILES {',
     '        if selected.len() > DREAM_USER_NOTE_MAX_FILES {', 'window_matches_ts'),
    # 19. Only U+017F is masked, so U+212A folds onto ASCII k again.
    ('clusters-kelvin-fold', js_value,
     r'''    if text.contains(['\u{017F}', '\u{212A}']) {
        std::borrow::Cow::Owned(text.replace(['\u{017F}', '\u{212A}'], "\u{FFFD}"))''',
     r'''    if text.contains(['\u{017F}']) {
        std::borrow::Cow::Owned(text.replace(['\u{017F}'], "\u{FFFD}"))''',
     'clusters_match_ts'),
]
env = dict(os.environ)
env['CARGO_TARGET_DIR'] = target
only = {name for name in os.environ.get('NEGATIVE_ONLY', '').split(',') if name}
unknown = only - {case[0] for case in cases}
assert not unknown, f'unknown NEGATIVE_ONLY cases: {sorted(unknown)}'


def assertion_failure(log):
    if 'could not compile' in log:
        return False
    if 'assertion `left == right` failed' in log:
        return True
    return any(f'{anchor}/' in log for anchor in assertion_anchors)


killed, survived = [], []
with mutating_sources(root, [helpers, js_value], baseline=baseline) as originals:
    base_cmd = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
                '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_dream_helpers']
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    run = run_owned(base_cmd, cwd=root, env=env, timeout=600)
    (out / 'baseline.log').write_text(slots.stdout + slots.stderr + run.stdout + run.stderr)
    if run.returncode != 0:
        print('BASELINE FAILED: unmutated suite must pass before mutations', flush=True)
        sys.exit(1)

    for name, path, before, after, test in cases:
        if only and name not in only:
            continue
        reason = None
        try:
            source = originals[path].decode()
            assert source.count(before) == 1, (name, source.count(before))
            slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
            path.write_text(source.replace(before, after))
            try:
                cmd = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
                       '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_dream_helpers',
                       test, '--', '--exact']
                run = run_owned(cmd, cwd=root, env=env, timeout=300)
                log = run.stdout + run.stderr
                (out / f'{name}.log').write_text(slots.stdout + slots.stderr + log)
                if not (run.returncode != 0 and assertion_failure(log)):
                    reason = 'did not fail a narrowed assertion'
            finally:
                path.write_bytes(originals[path])
        except Exception as error:
            reason = f'{type(error).__name__}: {error}'
        if reason is None:
            killed.append(name)
            print(f'KILLED   {name}: {test}', flush=True)
        else:
            survived.append((name, reason))
            print(f'SURVIVED {name}: {reason}', flush=True)
    for path, data in originals.items():
        path.write_bytes(data)
        assert path.read_bytes() == data, 'restoration failed: ' + str(path)
print(f'KILLED   {len(killed)}: ' + ', '.join(killed))
print(f'SURVIVED {len(survived)}: ' + ', '.join(name for name, _ in survived))
if survived:
    sys.exit(1)

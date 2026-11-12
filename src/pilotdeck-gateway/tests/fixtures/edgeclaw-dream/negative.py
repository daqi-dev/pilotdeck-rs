#!/usr/bin/env python3
"""Reversible product mutations for edgeclaw-dream.

Before any mutation the script runs the unmutated suite once and requires
exit code 0: a panic caused by the environment (for example a bad
``PDRS_BASELINE``) must not be mistaken for a killed mutant. A killed mutant
is the shared #813 criterion (#1087): the bound test's own ``FAILED`` status
line plus one of the oracle's ``{group}/`` anchors inside that test's own
panic payload. A marker anywhere else in the log — a sibling test's panic,
captured stdout, build output — is not evidence. ``could not compile`` is
never a kill.

Every case compiles and the original bytes are restored in ``finally``.
``mutating_sources`` demands that the gateway product tree match the baseline,
so an uncommitted authoring tree is refused; the coordinator runs this once
after committing. For the uncommitted tree, ``NEGATIVE_BASELINE=$(git stash
create)`` names a dangling commit of the current worktree (stage new files
first) and ``NEGATIVE_ONLY=a,b`` runs a sample. The default baseline remains
HEAD.

Every mutation anchor is checked against the baseline bytes *before* the
unmutated suite runs. A stale anchor (the product source moved on but this list
did not) is a harness defect, not a surviving mutant, so it prints ``STALE
ANCHOR`` and exits 2 instead of being counted as SURVIVED.

A surviving mutant does not abort the run: every case executes, then the script
prints KILLED/SURVIVED and exits 1 when anything survived. It never starts a
second build directory and cargo always uses the caller's ``CARGO_TARGET_DIR``.
The judge and its twin controls live at module level so
``test_negative.py`` can import them without running the suite.
"""
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed
from negative_process import (mutating_sources, run_owned, wait_for_slot,
                             write_source_atomic)

root = Path(__file__).resolve().parents[5]
fixtures = Path(__file__).resolve().parent
helpers = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/dream/helpers.rs'
js_value = root / 'src/pilotdeck-gateway/src/js_value.rs'

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
    #    Anchor only the operator: the receiver is `text.as_ref()` after the
    #    non-unicode /i fold, and that refactor must not stale the control.
    ('clusters-generic-and', helpers,
     '&& !semantic_pattern().is_match',
     '|| !semantic_pattern().is_match',
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


def assertion_failure(log, test):
    # #1087: only the named test's own FAILED status line plus the group
    # anchor inside that test's own panic payload kills (negative_panic
    # judges both). The old `anchor in log` also matched a sibling test's
    # panic, captured stdout and build output — the false-kill shapes the
    # twin controls in test_negative.py pin.
    if 'could not compile' in log:
        return False
    # #1087 fix-r5 (simplify #2): the FAILED summary requirement moved into
    # the shared named_test_killed default (require_summary=True).
    return named_test_killed(log, test,
                             [f'{anchor}/' for anchor in assertion_anchors])


def main(argv=None):
    argv = list(sys.argv if argv is None else argv)
    out = Path(argv[1]).resolve()
    target = os.environ.get('CARGO_TARGET_DIR')
    assert target, 'negative.py requires CARGO_TARGET_DIR from the caller'
    baseline = os.environ.get('NEGATIVE_BASELINE') or 'HEAD'
    out.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    env['CARGO_TARGET_DIR'] = target
    only = {name for name in os.environ.get('NEGATIVE_ONLY', '').split(',') if name}
    unknown = only - {case[0] for case in cases}
    assert not unknown, f'unknown NEGATIVE_ONLY cases: {sorted(unknown)}'

    killed, survived = [], []
    # Derive the snapshot set from the mutation table. The previous hand-written
    # [helpers, js_value] happened to cover every case, but the next added target
    # would raise KeyError here before any mutant ran (same defect as #773
    # edgeclaw-dream-runner).
    mutation_targets = list(dict.fromkeys(case[1] for case in cases))
    with mutating_sources(root, mutation_targets, baseline=baseline) as originals:
        # A changed product line that this list did not follow used to be reported
        # as SURVIVED (`AssertionError: (name, 0)`), hiding a broken control behind
        # the same label as a real surviving mutant. Refuse before compiling.
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
            print('STALE ANCHORS: negative.py is out of sync with the product source; refusing to run',
                  flush=True)
            return 2
        base_cmd = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
                    '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_dream_helpers']
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
        run = run_owned(base_cmd, cwd=root, env=env, timeout=600)
        (out / 'baseline.log').write_text(slots.stdout + slots.stderr + run.stdout + run.stderr)
        if run.returncode != 0:
            print('BASELINE FAILED: unmutated suite must pass before mutations', flush=True)
            return 1

        for name, path, before, after, test in cases:
            if only and name not in only:
                continue
            reason = None
            try:
                source = originals[path].decode()
                slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
                # #1087 fix-r2: every guarded-source write in the window is one
                # atomic rename (#1903 rule, shared with ``mutating_sources``) —
                # a truncating ``write_text`` killed mid-write leaves an empty
                # or half-written product source.
                write_source_atomic(
                    root, path, source.replace(before, after).encode(),
                    mode=path.stat().st_mode & 0o7777)
                try:
                    cmd = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
                           '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_dream_helpers',
                           test, '--', '--exact']
                    run = run_owned(cmd, cwd=root, env=env, timeout=300)
                    log = run.stdout + run.stderr
                    (out / f'{name}.log').write_text(slots.stdout + slots.stderr + log)
                    if not (run.returncode != 0 and assertion_failure(log, test)):
                        reason = 'did not fail a narrowed assertion'
                finally:
                    # Same #1087 fix-r2 window: the in-loop restore is also one
                    # rename, never a truncating write_bytes.
                    write_source_atomic(
                        root, path, originals[path],
                        mode=path.stat().st_mode & 0o7777)
            except Exception as error:
                reason = f'{type(error).__name__}: {error}'
            if reason is None:
                killed.append(name)
                print(f'KILLED   {name}: {test}', flush=True)
            else:
                survived.append((name, reason))
                print(f'SURVIVED {name}: {reason}', flush=True)
        for path, data in originals.items():
            write_source_atomic(root, path, data,
                                mode=path.stat().st_mode & 0o7777)
            assert path.read_bytes() == data, 'restoration failed: ' + str(path)
    print(f'KILLED   {len(killed)}: ' + ', '.join(killed))
    print(f'SURVIVED {len(survived)}: ' + ', '.join(name for name, _ in survived))
    return 1 if survived else 0


if __name__ == '__main__':
    raise SystemExit(main())

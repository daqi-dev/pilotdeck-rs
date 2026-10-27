#!/usr/bin/env python3
"""Reversible product mutations for edgeclaw-dream-run (#774).

Every mutant is a small product-source change in `dream/run.rs` or
`dream/runner.rs`. Before any mutation every *selected* target test runs once
on the unmutated source and must pass; a broken environment is not a kill. A
kill requires the named test itself to fail at the mutant's own ``#774 ...``
assertion message. Compile failures, unrelated panics and missing anchor
messages are INVALID, never KILLED. Stale anchors exit 2 instead of being
reported as SURVIVED. Every mutant runs, then the summary lists every
KILLED/SURVIVED/INVALID; any non-KILLED exits 1.

`mutating_sources` requires the product tree to match the baseline. The new
`run.rs`/test file are not in HEAD before the coordinator commits them, so a
normal author-tree run is expected to refuse; the author tree only runs
``--check-anchors`` plus a manual apply/restore sample. Never use a shadow git
or a second build directory. cargo always uses the caller's
``CARGO_TARGET_DIR`` and ``wait_for_slot``.
"""
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
run = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/dream/run.rs'
runner = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/dream/runner.rs'

mutations = [
    # 1. `isNoOp` forgets that a metadata update was applied: a real change
    #    becomes a no-op on the null-profile path (#774 result).
    ('noop-ignores-meta-update', run,
     '''            && !user_profile_updated
            && !meta_updated;''',
     '''            && !user_profile_updated
            && meta_updated;''',
     'run_single_null_profile'),
    # 2. `reviewedFiles` stops counting the General project nodes.
    ('reviewed-files-drops-general', run,
     '''            reviewed_files: workspace_entries.len() as f64
                + user_note_records.len() as f64
                + general_project_metas.len() as f64,''',
     '''            reviewed_files: workspace_entries.len() as f64
                + user_note_records.len() as f64
                + 0.0,''',
     'run_general_merge'),
    # 3. Absorbed user notes are no longer counted as deleted files.
    ('deleted-files-drops-notes', run,
     '''        let deleted_files = project_dream.deleted_files
            + feedback_dream.deleted_files
            + absorbed_user_note_ids.len() as f64;''',
     '''        let deleted_files = project_dream.deleted_files
            + feedback_dream.deleted_files
            + 0.0;''',
     'run_single_full'),
    # 4. The kept-notes warning changes wording.
    ('kept-profile-wording', run,
     '"Dream kept the selected user notes because the rewritten profile matched the existing profile."',
     '"Dream kept the selected user notes because the rewritten profile matched."',
     'run_single_kept_profile'),
    # 4. The caught rewrite-request failure loses its TS message prefix.
    ('rewrite-error-prefix', run,
     '"Dream skipped user profile rewrite because the rewrite request failed: "',
     '"Dream skipped user profile rewrite because the request failed: "',
     'run_single_rewrite_error'),
    # 5. The empty-work summary changes wording.
    ('default-summary-wording', run,
     'const DEFAULT_SUMMARY: &str = "Dream completed without requiring any memory changes.";',
     'const DEFAULT_SUMMARY: &str = "Dream completed without memory changes.";',
     'run_single_null_profile'),
    # 6. #915: a refine error is swallowed, so `runCategoryDream` returns Ok
    #    and the caller sees a successful partial dream.
    ('refine-error-swallowed', runner,
     '        loop_result?;',
     '        loop_result.ok();',
     'category_warning_then_error'),
    # 7. #915: the error-exit sync drops the warning that TS keeps through the
    #    shared array reference.
    ('error-exit-warning-sync', runner,
     '''        sync_plan_output_warnings(
            trace,
            plan_step_index,
            &concat(&[&kind_label, &"-cluster-plan-output".into()]),
            &dropped_warnings,
        );''',
     '''        sync_plan_output_warnings(
            trace,
            plan_step_index,
            &concat(&[&kind_label, &"-cluster-plan-output".into()]),
            &[],
        );''',
     'category_warning_then_error'),
]

fragments = {
    'noop-ignores-meta-update': '#774 result run-single-null-profile',
    'reviewed-files-drops-general': '#774 result run-general-merge',
    'deleted-files-drops-notes': '#774 result run-single-full',
    'kept-profile-wording': '#774 result run-single-kept-profile',
    'rewrite-error-prefix': '#774 result run-single-rewrite-error',
    'default-summary-wording': '#774 result run-single-null-profile',
    'refine-error-swallowed': '#774 error category-warning-then-error',
    'error-exit-warning-sync': '#774 trace category-warning-then-error',
}

if '--check-anchors' in sys.argv:
    sources = {path: path.read_text() for _name, path, _before, _after, _test in mutations}
    bad = [(name, path, sources[path].count(before))
           for name, path, before, _after, _test in mutations
           if sources[path].count(before) != 1]
    for name, path, count in bad:
        print(f'STALE ANCHOR {name}: {count} matches in {path.relative_to(root)}')
    if bad:
        sys.exit(2)
    print(f'OK {len(mutations)} anchors in {len(sources)} files')
    sys.exit(0)

out = Path(sys.argv[1]).resolve()
target = os.environ.get('CARGO_TARGET_DIR')
assert target, 'negative.py requires CARGO_TARGET_DIR from the caller'
baseline = os.environ.get('NEGATIVE_BASELINE', 'HEAD')
out.mkdir(parents=True, exist_ok=True)
env = dict(os.environ)
env['CARGO_TARGET_DIR'] = target
# The provenance helper must not require the whole baseline checkout for a
# source-mutation run; unset PDRS_BASELINE is the documented no-check mode.
env.pop('PDRS_BASELINE', None)
only = {name for name in os.environ.get('NEGATIVE_ONLY', '').split(',') if name}
unknown = only - {case[0] for case in mutations}
assert not unknown, f'unknown NEGATIVE_ONLY cases: {sorted(unknown)}'


def run_suite(test):
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    command = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
               '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_dream_run',
               test, '--', '--exact']
    run = run_owned(command, cwd=root, env=env, timeout=600)
    return slots, run, run.stdout + run.stderr


def failure_is_bound(log, fragment, test):
    if 'could not compile' in log:
        return False
    if f'test {test} ... FAILED' not in log:
        return False
    return fragment in log


killed, survived, invalid = [], [], []
mutation_targets = list(dict.fromkeys(case[1] for case in mutations))
with mutating_sources(root, mutation_targets, baseline=baseline) as originals:
    stale = []
    for name, path, before, _after, _test in mutations:
        count = originals[path].decode().count(before)
        if count != 1:
            stale.append((name, path.relative_to(root).as_posix(), count))
    if stale:
        report = '\n'.join(f'STALE ANCHOR {name}: {count} matches in {relative}'
                           for name, relative, count in stale) + '\n'
        (out / 'stale-anchors.log').write_text(report)
        print(report, end='')
        print('STALE ANCHORS: negative.py is out of sync with the product source; refusing to run')
        sys.exit(2)

    selected = [case for case in mutations if not only or case[0] in only]
    target_tests = list(dict.fromkeys(case[4] for case in selected))
    baseline_log = []
    baseline_failures = []
    for test in target_tests:
        try:
            slots, run_result, log = run_suite(test)
            baseline_log.append(f'== test {test}\n{slots.stdout}{slots.stderr}{log}')
            if run_result.returncode != 0:
                baseline_failures.append(test)
        except Exception as error:
            baseline_log.append(f'== test {test}\n{type(error).__name__}: {error}\n')
            baseline_failures.append(f'{test} ({type(error).__name__})')
    (out / 'baseline.log').write_text(''.join(baseline_log))
    if baseline_failures:
        print('BASELINE FAILED: unmutated target tests must pass before mutations: '
              + ', '.join(baseline_failures))
        sys.exit(1)

    for name, path, before, after, test in mutations:
        if only and name not in only:
            continue
        try:
            source = originals[path].decode()
            path.write_text(source.replace(before, after))
            slots, run_result, log = run_suite(test)
            (out / f'{name}.log').write_text(slots.stdout + slots.stderr + log)
            if 'could not compile' in log:
                invalid.append((name, 'compile failure'))
            elif run_result.returncode == 0:
                survived.append((name, 'test passed'))
            elif not failure_is_bound(log, fragments[name], test):
                invalid.append((name, 'failed without the bound assertion message'))
            else:
                killed.append(name)
        except Exception as error:
            invalid.append((name, f'{type(error).__name__}: {error}'))
        finally:
            path.write_bytes(originals[path])
        if killed and killed[-1] == name:
            print(f'KILLED   {name}: {fragments[name]}', flush=True)
        elif survived and survived[-1][0] == name:
            print(f'SURVIVED {name}: {survived[-1][1]}', flush=True)
        elif invalid and invalid[-1][0] == name:
            print(f'INVALID  {name}: {invalid[-1][1]}', flush=True)

    for path, data in originals.items():
        path.write_bytes(data)
        assert path.read_bytes() == data, 'restoration failed: ' + str(path)

print(f'KILLED   {len(killed)}: ' + ', '.join(killed))
print(f'SURVIVED {len(survived)}: ' + ', '.join(f'{name} ({reason})' for name, reason in survived))
print(f'INVALID  {len(invalid)}: ' + ', '.join(f'{name} ({reason})' for name, reason in invalid))
if survived or invalid:
    sys.exit(1)

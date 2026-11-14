#!/usr/bin/env python3
"""Reversible product mutations for edgeclaw-dream-runner (#773).

Every mutant is a one-line change to `dream/runner.rs`. Before any mutation
every *selected* target test runs once on the unmutated source and must pass;
a broken environment is not a kill. A kill requires the named test itself to
fail at the mutant's own ``#773 ...`` assertion message. Compile failures,
unrelated panics and missing anchor messages are INVALID, never KILLED. Stale
anchors exit 2 instead of being reported as SURVIVED. Every mutant runs, then
the script prints the aggregate KILLED/SURVIVED/INVALID lists and exits 1 if
anything was not killed.

`mutating_sources` requires the product tree to match the baseline. The fix-r1
and fix-r2 product changes are not committed before the coordinator commits
them, so a normal run is expected to refuse here; the author tree only runs
``--check-anchors``.
Never use a shadow git or a second build directory. cargo always uses the
caller's ``CARGO_TARGET_DIR`` and ``wait_for_slot``.
"""
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed
from negative_process import (dead_stream_print, mutating_sources,
                              new_run_token, run_owned, wait_for_slot,
                              write_source_atomic)

root = Path(__file__).resolve().parents[5]
runner = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/dream/runner.rs'
run_scratch = root / 'src/pilotdeck-gateway/tests/fixtures/_shared/run_scratch.mjs'
observe = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-dream-runner/observe.mts'

mutations = [
    # 1. The header step's i18n key loses the trace prefix.
    ('header-i18n-key', runner,
     '&concat(&[&"trace.step.".into(), &header_step_kind.into()]),',
     '&concat(&[&"trace.step.x".into(), &header_step_kind.into()]),',
     'category_project_cluster'),
    # 2. An empty header scan reports success instead of skipped.
    ('header-empty-status', runner,
     '''            &(if !sorted_entries.is_empty() {
                "success"
            } else {
                "skipped"
            })
            .into(),''',
     '            &JsString::from("success"),',
     'category_empty'),
    # 3. The planned-cluster count collapses to zero.
    ('planned-cluster-count', runner,
     'planned_clusters: clusters.len() as f64,',
     'planned_clusters: 0.0,',
     'category_project_cluster'),
    # 4. The write preview cap drifts by 210 UTF-16 units.
    ('preview-limit', runner,
     'preview_markdown(&content, 220.0)',
     'preview_markdown(&content, 10.0)',
     'category_project_cluster'),
    # 5. Only the first member of a merged cluster is recorded as deleted.
    ('delete-mutation-count', runner,
     'for path in &members {',
     'for path in members.iter().take(1) {',
     'category_project_cluster'),
    # 6. Feedback writes are tagged as project writes.
    ('write-candidate-type', runner,
     'Some(&JsValue::String(kind.clone())),',
     'Some(&JsValue::String("project".into())),',
     'category_feedback_cluster'),
    # 7. The "no refined file" warning changes wording.
    ('refine-null-wording', runner,
     '&"Dream could not synthesize a refined ".into(),',
     '&"Dream could not create a refined ".into(),',
     'category_refine_null'),
    # 8. The merge planning failure prefix changes wording.
    ('merge-error-prefix', runner,
     '&"Dream skipped General project merge planning because the model request failed: ".into(),',
     '&"Dream could not plan General project merges because the model request failed: ".into(),',
     'merge_plan_error'),
    # 9. One General meta is enough to enter merge planning.
    ('merge-single-threshold', runner,
     'plan_summary: if metas.len() < 2 {',
     'plan_summary: if metas.len() < 1 {',
     'merge_skip_single'),
    # 10. The deleted project mutation loses its action name.
    ('delete-project-action', runner,
     'mutation_object.insert("action".into(), "delete_project".into());',
     'mutation_object.insert("action".into(), "delete".into());',
     'merge_basic'),
    # 11. Relinked files no longer produce write mutations.
    ('relinked-write-mutations', runner,
     'for relative_path in &relinked_files {',
     'for relative_path in relinked_files.iter().take(0) {',
     'merge_basic'),
    # 12. The scoped category label drops its separator.
    ('scoped-label-separator', runner,
     '&" · ".into(),',
     '&" - ".into(),',
     'category_general_scope'),
    # 13. The default meta-review reason changes wording.
    ('meta-default-reason', runner,
     '"Dream kept the current project metadata unchanged.".into();',
     '"Dream left the current project metadata unchanged.".into();',
     'meta_review_empty'),
    # 14. The meta-review write preview loses a space around its separator.
    ('meta-preview-separator', runner,
     '''&" | ".into(),
                    &string_field(&next_meta, "status").unwrap_or_default(),''',
     '''&" |".into(),
                    &string_field(&next_meta, "status").unwrap_or_default(),''',
     'meta_review_update'),
    # 15. The mirror meta review falls back to the local-only entry list.
    ('meta-review-mirror-branch', runner,
     'if source_kind.equals_str("workspace_external_mirror") {',
     'if source_kind.equals_str("never_mirror") {',
     'meta_review_mirror'),
    # 16. The merge planning warning loses its "planning" wording.
    ('merge-warning-prefix', runner,
     '&"[clawxmemory] General project meta merge planning failed: ".into(),',
     '&"[clawxmemory] General project meta merge failed: ".into(),',
     'merge_plan_error'),
    # 17. `String(error)` keeps ": " when the message is empty.
    ('merge-error-empty-message', runner,
     '''        if message.is_empty() {
            return name;
        }''',
     '''        if message.is_empty() && !message.is_empty() {
            return name;
        }''',
     'merge_plan_error_empty_message'),
    # 18. `String(error)` prefixes ": " even when the name is empty.
    ('merge-error-empty-name', runner,
     '''        if name.is_empty() {
            return message;
        }''',
     '''        if name.is_empty() && !name.is_empty() {
            return message;
        }''',
     'merge_plan_error_empty_name'),
    # 19. The warning's message is decoded lossily, replacing a lone surrogate.
    ('merge-error-lossy-message', runner,
     'let message = self.js_message();',
     'let message = JsString::from(String::from_utf16_lossy(self.js_message().units()));',
     'merge_plan_error_lone_surrogate'),
    # 20. Cluster-load warnings never reach the plan-output trace detail.
    ('plan-output-missing-record-sync', runner,
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
     'category_missing_record'),
    # 21. The summary KV is recomputed instead of keeping its push-time count.
    ('plan-dropped-kv-recomputed', runner,
     '''                "droppedClusters".into(),
                JsValue::Number(dropped_warnings.len() as f64),''',
     '''                "droppedClusters".into(),
                JsValue::Number((dropped_warnings.len() + 1) as f64),''',
     'category_missing_record'),
    # 22. Every scratch allocation reuses one fixed child instead of mkdtemp.
    ('run-scratch-fixed-root', run_scratch,
     '  return {root, owned: mkdtempSync(join(root, prefix))};',
     '''  const fixed = join(root, prefix);
  mkdirSync(fixed, {recursive: true});
  return {root, owned: fixed};''',
     'scratch_root_is_run_exclusive'),
    # 23. The generator reopens the shared fixed stage path at startup.
    ('observe-fixed-scratch', observe,
     '''  const scratch = createRunScratch(
    process.env.PDRS_RUNNER_SCRATCH || tmpdir(),
    'pdrs-dream-runner-observe-',
  );''',
     "  const scratch = {root: '/tmp', owned: '/tmp/pdrs-dream-runner-observe'};",
     'scratch_root_is_run_exclusive'),
    # 24. The ownership guard is gone, so the run parent itself is deleted.
    ('run-scratch-parent-delete', run_scratch,
     '''  if (resolve(owned) === resolve(root)) {
    throw new Error(`#773 remove refused: ${owned} is the run parent, not an owned child`);
  }''',
     '',
     'scratch_root_is_run_exclusive'),
    # 25. P2-2: the allocator call is retained, but `collectCases` is fed the
    #     fixed stage path. The parent-only fixed-scratch mutant above dies on
    #     the allocation assertion, so it cannot cover this gap.
    ('observe-fixed-path-retained-allocator', observe,
     '    return await collectCases(baseline, scratch.owned);',
     "    return await collectCases(baseline, '/tmp/pdrs-dream-runner-observe');",
     'scratch_root_is_run_exclusive'),
    # 26. #915: the warning backfill only runs on the Ok exit, so a later
    #     cluster's refine error still leaves the plan-output detail stale.
    ('plan-output-error-sync', runner,
     '''        sync_plan_output_warnings(
            trace,
            plan_step_index,
            &concat(&[&kind_label, &"-cluster-plan-output".into()]),
            &dropped_warnings,
        );
        loop_result?;''',
     '''        if loop_result.is_ok() {
            sync_plan_output_warnings(
                trace,
                plan_step_index,
                &concat(&[&kind_label, &"-cluster-plan-output".into()]),
                &dropped_warnings,
            );
        }
        loop_result?;''',
     'category_refine_error'),
]

fragments = {
    'header-i18n-key': '#773 trace category-project-cluster',
    'header-empty-status': '#773 trace category-empty',
    'planned-cluster-count': '#773 result category-project-cluster',
    'preview-limit': '#773 trace category-project-cluster',
    'delete-mutation-count': '#773 trace category-project-cluster',
    'write-candidate-type': '#773 trace category-feedback-cluster',
    'refine-null-wording': '#773 trace category-refine-null',
    'merge-error-prefix': '#773 result merge-plan-error',
    'merge-single-threshold': '#773 result merge-skip-single',
    'delete-project-action': '#773 trace merge-basic',
    'relinked-write-mutations': '#773 trace merge-basic',
    'scoped-label-separator': '#773 trace category-general-scope',
    'meta-default-reason': '#773 result meta-review-empty',
    'meta-preview-separator': '#773 trace meta-review-update',
    'meta-review-mirror-branch': '#773 trace meta-review-mirror',
    'merge-warning-prefix': '#773 logs merge-plan-error',
    'merge-error-empty-message': '#773 logs merge-plan-error-empty-message',
    'merge-error-empty-name': '#773 logs merge-plan-error-empty-name',
    'merge-error-lossy-message': '#773 logs merge-plan-error-lone-surrogate',
    'plan-output-missing-record-sync': '#773 trace category-missing-record',
    'plan-dropped-kv-recomputed': '#773 trace category-missing-record',
    'run-scratch-fixed-root': '#773 scratch roots must be distinct mkdtemp dirs',
    'observe-fixed-scratch': '#773 dream-runner observe must allocate its scratch through createRunScratch',
    'run-scratch-parent-delete': '#773 remove refused: the run parent must not be removed',
    'observe-fixed-path-retained-allocator': '#773 dream-runner observe must not use the shared fixed stage path',
    'plan-output-error-sync': '#915 plan-output warning category-refine-error',
}

def failure_is_bound(log, fragment, test):
    # #1087: only the named test's own FAILED status line plus the bound
    # fragment inside that test's own panic payload kills (negative_panic
    # judges both). The old whole-log `fragment in log` also matched a
    # sibling test's panic, captured stdout and build output — the
    # false-kill shapes the twin controls in test_negative.py pin.
    if 'could not compile' in log:
        return False
    # #1087 fix-r5 (simplify #2): the FAILED summary requirement moved
    # into the shared named_test_killed default (require_summary=True).
    return named_test_killed(log, test, fragment)



def main(argv=None):
    argv = list(sys.argv if argv is None else argv)
    if '--check-anchors' in argv:
        sources = {path: path.read_text() for _name, path, _before, _after, _test in mutations}
        bad = [(name, path, sources[path].count(before))
               for name, path, before, _after, _test in mutations
               if sources[path].count(before) != 1]
        if bad:
            # #1087 fix-r8 (S1-a): the shared guarded print — a dead stdout
            # must not turn this exit-2 refusal into a crash (rc=1) or a
            # shutdown-flush 120; the code is the contract.
            for name, path, count in bad:
                dead_stream_print(f'STALE ANCHOR {name}: {count} matches in {path.relative_to(root)}')
            sys.exit(2)
        # #1087 fix-r8 (S1-a, concurrency #2): the green --check-anchors
        # print is the same guarded family — exit 0 must survive a dead
        # stdout.
        dead_stream_print(f'OK {len(mutations)} anchors in {len(sources)} files')
        sys.exit(0)

    out = Path(argv[1]).resolve()
    target = os.environ.get('CARGO_TARGET_DIR')
    assert target, 'negative.py requires CARGO_TARGET_DIR from the caller'
    baseline = os.environ.get('NEGATIVE_BASELINE') or 'HEAD'
    out.mkdir(parents=True, exist_ok=True)
    # #1087 fix-r8 (S1-b/S2-c): the per-run token keeps the evidence log
    # names of two overlapping runs on this reused directory from
    # truncating each other, and survives pid recycling (time_ns).
    run_token = new_run_token()
    env = dict(os.environ)
    env['CARGO_TARGET_DIR'] = target
    only = {name for name in os.environ.get('NEGATIVE_ONLY', '').split(',') if name}
    unknown = only - {case[0] for case in mutations}
    assert not unknown, f'unknown NEGATIVE_ONLY cases: {sorted(unknown)}'


    def run_suite(test):
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
        command = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
                   '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_dream_runner',
                   test, '--', '--exact']
        run = run_owned(command, cwd=root, env=env, timeout=600)
        return slots, run, run.stdout + run.stderr

    killed, survived, invalid = [], [], []
    # Derive the snapshot set from the *selected* mutation table itself. A
    # hand-written list went stale when fix-r3 added mutants for
    # _shared/run_scratch.mjs and observe.mts: mutating_sources only snapshotted
    # runner.rs, so the first `originals[path]` lookup raised KeyError and every
    # later mutant was skipped. Selecting the paths here also keeps a
    # NEGATIVE_ONLY run usable while an unrelated, not-yet-committed fixture file
    # is being edited; a full run still snapshots and checks every target.
    selected = [case for case in mutations if not only or case[0] in only]
    mutation_targets = list(dict.fromkeys(case[1] for case in selected))
    with mutating_sources(root, mutation_targets, baseline=baseline) as originals:
        stale = []
        for name, path, before, _after, _test in selected:
            count = originals[path].decode().count(before)
            if count != 1:
                stale.append((name, path.relative_to(root).as_posix(), count))
        if stale:
            report = '\n'.join(f'STALE ANCHOR {name}: {count} matches in {relative}'
                               for name, relative, count in stale) + '\n'
            (out / f'stale-anchors.{run_token}.log').write_text(report)
            # #1087 fix-r8 (S1-a): the shared guarded print — a dead stdout
            # must not turn this exit-2 refusal into a crash (rc=1) or a
            # shutdown-flush 120; the report is already on disk in the
            # stale-anchors log, the code is the contract.
            dead_stream_print(report, end='')
            dead_stream_print('STALE ANCHORS: negative.py is out of sync with the product source; refusing to run')
            sys.exit(2)

        # Prove every *target* test passes on the unmutated source before any
        # mutation. Checking one fixed test let an unrelated pre-existing failure
        # in a selected mutant's target satisfy the bound message and be counted
        # as KILLED. Run each selected target test once, deduplicated.
        target_tests = list(dict.fromkeys(case[4] for case in selected))
        baseline_log = []
        baseline_failures = []
        for test in target_tests:
            try:
                slots, run, log = run_suite(test)
                baseline_log.append(f'== test {test}\n{slots.stdout}{slots.stderr}{log}')
                if run.returncode != 0:
                    baseline_failures.append(test)
            except Exception as error:
                baseline_log.append(f'== test {test}\n{type(error).__name__}: {error}\n')
                baseline_failures.append(f'{test} ({type(error).__name__})')
        (out / f'baseline.{run_token}.log').write_text(''.join(baseline_log))
        if baseline_failures:
            print('BASELINE FAILED: unmutated target tests must pass before mutations: '
                  + ', '.join(baseline_failures))
            sys.exit(1)

        for name, path, before, after, test in selected:
            try:
                source = originals[path].decode()
                # #1087 fix-r2: every guarded-source write in the window is
                # one atomic rename (#1903 rule, shared with
                # ``mutating_sources``) — a truncating ``write_text`` killed
                # mid-write leaves an empty or half-written product source.
                write_source_atomic(root, path,
                                     source.replace(before, after).encode(),
                                     mode=path.stat().st_mode & 0o7777)
                slots, run, log = run_suite(test)
                (out / f'{name}.{run_token}.log').write_text(slots.stdout + slots.stderr + log)
                if 'could not compile' in log:
                    invalid.append((name, 'compile failure'))
                elif run.returncode == 0:
                    survived.append((name, 'test passed'))
                elif not failure_is_bound(log, fragments[name], test):
                    invalid.append((name, 'failed without the bound assertion message'))
                else:
                    killed.append(name)
            except Exception as error:
                invalid.append((name, f'{type(error).__name__}: {error}'))
            finally:
                # Same #1087 fix-r2 window: the in-loop restore is also one
                # rename, never a truncating write_bytes.
                write_source_atomic(root, path, originals[path],
                                     mode=path.stat().st_mode & 0o7777)
            if killed and killed[-1] == name:
                print(f'KILLED   {name}: {fragments[name]}', flush=True)
            elif survived and survived[-1][0] == name:
                print(f'SURVIVED {name}: {survived[-1][1]}', flush=True)
            elif invalid and invalid[-1][0] == name:
                print(f'INVALID  {name}: {invalid[-1][1]}', flush=True)

        for path, data in originals.items():
            # Same #1087 fix-r2 window: the final restore is also one rename,
            # never a truncating write_bytes.
            write_source_atomic(root, path, data,
                                 mode=path.stat().st_mode & 0o7777)
            assert path.read_bytes() == data, 'restoration failed: ' + str(path)

    print(f'KILLED   {len(killed)}: ' + ', '.join(killed))
    print(f'SURVIVED {len(survived)}: ' + ', '.join(f'{name} ({reason})' for name, reason in survived))
    print(f'INVALID  {len(invalid)}: ' + ', '.join(f'{name} ({reason})' for name, reason in invalid))
    if survived or invalid:
        sys.exit(1)


if __name__ == '__main__':
    main()

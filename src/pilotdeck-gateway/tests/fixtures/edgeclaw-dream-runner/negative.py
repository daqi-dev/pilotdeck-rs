#!/usr/bin/env python3
"""Reversible product mutations for edgeclaw-dream-runner (#773).

Every mutant is a one-line change to `dream/runner.rs`. Before any mutation the
unmutated suite runs once and must exit 0; a broken environment is not a kill.
A kill requires the named test to fail at the mutant's own ``#773 ...``
assertion message. Compile failures, unrelated panics and missing anchor
messages are INVALID, never KILLED. Stale anchors exit 2 instead of being
reported as SURVIVED. Every mutant runs, then the script prints the aggregate
KILLED/SURVIVED/INVALID lists and exits 1 if anything was not killed.

`mutating_sources` requires the product tree to match the baseline. The new
`dream/runner.rs` is not in HEAD before the coordinator commits it, so a normal
run is expected to refuse here; the author tree only runs ``--check-anchors``.
Never use a shadow git or a second build directory. cargo always uses the
caller's ``CARGO_TARGET_DIR`` and ``wait_for_slot``.
"""
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
runner = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/dream/runner.rs'

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
}

if '--check-anchors' in sys.argv:
    source = runner.read_text()
    bad = [(name, source.count(before)) for name, _path, before, _after, _test in mutations
           if source.count(before) != 1]
    for name, count in bad:
        print(f'STALE ANCHOR {name}: {count} matches in {runner.relative_to(root)}')
    if bad:
        sys.exit(2)
    print(f'OK {len(mutations)} anchors in {runner.relative_to(root)}')
    sys.exit(0)

out = Path(sys.argv[1]).resolve()
target = os.environ.get('CARGO_TARGET_DIR')
assert target, 'negative.py requires CARGO_TARGET_DIR from the caller'
baseline = os.environ.get('NEGATIVE_BASELINE', 'HEAD')
out.mkdir(parents=True, exist_ok=True)
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


def failure_is_bound(log, fragment, test):
    if 'could not compile' in log:
        return False
    if f'test {test} ... FAILED' not in log and 'panicked at' not in log:
        return False
    return fragment in log


killed, survived, invalid = [], [], []
with mutating_sources(root, [runner], baseline=baseline) as originals:
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

    slots, run, log = run_suite('category_project_cluster')
    (out / 'baseline.log').write_text(slots.stdout + slots.stderr + log)
    if run.returncode != 0:
        print('BASELINE FAILED: unmutated category_project_cluster must pass before mutations')
        sys.exit(1)

    for name, path, before, after, test in mutations:
        if only and name not in only:
            continue
        try:
            source = originals[path].decode()
            path.write_text(source.replace(before, after))
            slots, run, log = run_suite(test)
            (out / f'{name}.log').write_text(slots.stdout + slots.stderr + log)
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

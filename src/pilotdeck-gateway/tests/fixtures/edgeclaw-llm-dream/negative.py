#!/usr/bin/env python3
"""Reversible product mutations for edgeclaw-llm-dream (#771).

Every mutant is a one-line change to `llm/dream.rs`. Before any mutation the
unmutated suite runs once and must exit 0; a broken environment is not a kill.
A kill requires the test to fail at the mutant's own ``#771 ...`` assertion
message. Compile failures, unrelated panics and missing anchor messages are
INVALID, never KILLED. Stale anchors exit 2 instead of being reported as
SURVIVED. Every mutant runs, then the script prints KILLED/SURVIVED and exits 1
if anything survived.

`mutating_sources` requires the product tree to match the baseline. The new
`llm/dream.rs` is not in HEAD before the coordinator commits it, so the normal
run is expected to refuse here; this tree can run samples with
``NEGATIVE_BASELINE=$(git stash create)`` after staging the new files. Never
use a shadow git or a second build directory. cargo always uses the caller's
``CARGO_TARGET_DIR`` and ``wait_for_slot``.
"""
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
fixtures = Path(__file__).resolve().parent
dream = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/llm/dream.rs'

mutations = [
    # 1. Cluster header description cap drifts by one UTF-16 unit.
    ('cluster-description-limit', dream,
     'truncate_for_prompt(required_string(header, "description")?, 220.0)',
     'truncate_for_prompt(required_string(header, "description")?, 221.0)',
     'wire_matrix'),
    # 2. Unknown relative paths are no longer filtered before the result.
    ('cluster-allowed-filter', dream,
     '            && allowed_relative_paths.contains(&member)\n',
     '',
     'wire_matrix'),
    # 3. Duplicate project ids are no longer deduped. Keep `seen` in the
    #    closure: a bare `|_| true` leaves the set's element type
    #    unconstrained (E0282), which is a compile failure, not a kill.
    ('merge-dedupe', dream,
     '.filter(|value| seen.insert(value.clone()))',
     '.filter(|value| seen.insert(value.clone()) || true)',
     'wire_matrix'),
    # 4. A blank status falls back to the wrong literal.
    ('meta-fallback-active', dream,
     '        JsString::from("active")',
     '        JsString::from("inactive")',
     'wire_matrix'),
    # 5. The profile body heading loses a character.
    ('profile-heading', dream,
     'let mut units = "## 身份背景\\n".encode_utf16().collect::<Vec<_>>();',
     'let mut units = "## 身份\\n".encode_utf16().collect::<Vec<_>>();',
     'wire_matrix'),
    # 6. Existing-profile prompt cap drifts by one UTF-16 unit.
    ('profile-existing-limit', dream,
     'truncate(content, 3_200.0)',
     'truncate(content, 3_201.0)',
     'wire_matrix'),
    # 7. Refined markdown no longer trims its leading/trailing whitespace.
    ('refine-markdown-trim', dream,
     '.map(|value| JsString::from_utf16(js_trim_units(value.units()).to_vec()))',
     '.map(|value| value)',
     'wire_matrix'),
    # 8. The missing-summary fallback changes wording.
    ('cluster-summary-fallback', dream,
     '"Dream {} cluster plan completed."',
     '"Dream {} cluster plan planned."',
     'wire_matrix'),
    # 9. `normalizeBoolean` fallback flips for non-boolean strings.
    ('review-should-update', dream,
     'normalize_boolean(value_or_undefined(payload, "should_update"), false)',
     'normalize_boolean(value_or_undefined(payload, "should_update"), true)',
     'wire_matrix'),
    # 10. The envelope throw loses its TS message.
    ('parse-envelope-error', dream,
     'LlmError::new(error.message())',
     'LlmError::new("wrong envelope error")',
     'wire_matrix'),
    # 11. A single cluster header is enough to call the model.
    ('cluster-early-threshold', dream,
     '        if headers.len() < 2 {',
     '        if headers.len() < 1 {',
     'wire_matrix'),
    # 12. An empty refine record list still calls the model.
    ('refine-early-threshold', dream,
     '        if records.is_empty() {',
     '        if false {',
     'wire_matrix'),
    # 13. A single General project meta is enough to plan merges.
    ('merge-early-threshold', dream,
     '        if array_field(input, "projectMetas").len() < 2 {',
     '        if array_field(input, "projectMetas").len() < 1 {',
     'wire_matrix'),
    # 14. A project-only candidate list still triggers a profile rewrite.
    ('rewrite-no-user', dream,
     '                    .is_some_and(|kind| kind.equals_str("user"))',
     '                    .is_some_and(|_| true)',
     'wire_matrix'),
]

fragments = {
    'cluster-description-limit': '#771 debug planDreamClusters#0',
    'cluster-allowed-filter': '#771 result planDreamClusters#0',
    'merge-dedupe': '#771 result planGeneralProjectMetaMerges#0',
    'meta-fallback-active': '#771 result reviewDreamProjectMeta#0',
    'profile-heading': '#771 result rewriteUserProfile#0',
    'profile-existing-limit': '#771 debug rewriteUserProfile#0',
    'refine-markdown-trim': '#771 result refineDreamCluster#0',
    'cluster-summary-fallback': '#771 result planDreamClusters#1',
    'review-should-update': '#771 result reviewDreamProjectMeta#1',
    'parse-envelope-error': '#771 error planDreamClusters#3',
    'cluster-early-threshold': '#771 result planDreamClusters#2',
    'refine-early-threshold': '#771 result refineDreamCluster#2',
    'merge-early-threshold': '#771 result planGeneralProjectMetaMerges#1',
    'rewrite-no-user': '#771 debug rewriteUserProfile#2',
}

if '--check-anchors' in sys.argv:
    source = dream.read_text()
    bad = [(name, source.count(before)) for name, _path, before, _after, _test in mutations
           if source.count(before) != 1]
    for name, count in bad:
        print(f'STALE ANCHOR {name}: {count} matches in {dream.relative_to(root)}')
    if bad:
        sys.exit(2)
    print(f'OK {len(mutations)} anchors in {dream.relative_to(root)}')
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


def run_suite():
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    command = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
               '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_llm_dream',
               'wire_matrix', '--', '--exact']
    run = run_owned(command, cwd=root, env=env, timeout=600)
    return slots, run, run.stdout + run.stderr


def failure_is_bound(log, fragment):
    if 'could not compile' in log:
        return False
    if 'test wire_matrix ... FAILED' not in log and 'panicked at' not in log:
        return False
    return fragment in log


killed, survived, invalid = [], [], []
with mutating_sources(root, [dream], baseline=baseline) as originals:
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

    slots, run, log = run_suite()
    (out / 'baseline.log').write_text(slots.stdout + slots.stderr + log)
    if run.returncode != 0:
        print('BASELINE FAILED: unmutated suite must pass before mutations')
        sys.exit(1)

    for name, path, before, after, _test in mutations:
        if only and name not in only:
            continue
        try:
            source = originals[path].decode()
            path.write_text(source.replace(before, after))
            slots, run, log = run_suite()
            (out / f'{name}.log').write_text(slots.stdout + slots.stderr + log)
            if 'could not compile' in log:
                invalid.append((name, 'compile failure'))
            elif run.returncode == 0:
                survived.append((name, 'test passed'))
            elif not failure_is_bound(log, fragments[name]):
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

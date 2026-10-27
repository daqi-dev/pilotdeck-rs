#!/usr/bin/env python3
"""Reversible product mutations for edgeclaw-llm-dream (#771).

Every mutant is a one-line change to `llm/dream.rs` or the shared
`edgeclaw/text.rs` JSON projection. Before any mutation the unmutated suite runs
once and must exit 0; a broken environment is not a kill. A kill requires the
test to fail at the mutant's own ``#771 ...`` assertion message. Compile
failures, unrelated panics and missing anchor messages are INVALID, never
KILLED. Stale anchors exit 2 instead of being reported as SURVIVED. Every mutant
runs, then the script prints KILLED/SURVIVED and exits 1 if anything survived.

`mutating_sources` requires the product tree to match the baseline. The changed
product files are not in HEAD before the coordinator commits them, so the normal
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
text = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/text.rs'

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
    # 15. fix-r2 P1-1: inner JSON decoding becomes lossy again.
    ('parse-model-json-utf16', dream,
     'let text = json_text(envelope.units());',
     'let text = String::from_utf16_lossy(envelope.units());',
     'wire_matrix'),
    # 16. fix-r2 P1-1: the SyntaxError diagnostic loses the original units.
    ('parse-model-json-error-units', dream,
     'json_syntax::parse_error_units(envelope.units())',
     'json_syntax::parse_error_units(&String::from_utf16_lossy(envelope.units()).encode_utf16().collect::<Vec<_>>())',
     'wire_matrix'),
    # 17. fix-r2 P1-2: U+2028/U+2029 stop being ECMAScript line terminators.
    ('markdown-line-terminators', dream,
     r'''    value = value.replace(['\u{2028}', '\u{2029}'], "\n");''',
     '',
     'wire_matrix'),
    # 18. fix-r2 P2-1: the cluster-plan default timeout drifts.
    ('timeout-plan-default', dream,
     'const DEFAULT_DREAM_CLUSTER_PLAN_TIMEOUT_MS: f64 = 180_000.0;',
     'const DEFAULT_DREAM_CLUSTER_PLAN_TIMEOUT_MS: f64 = 170_000.0;',
     'timeout_matrix'),
    # 19. fix-r2 P2-1: an explicit timeoutMs is ignored.
    ('timeout-explicit-override', dream,
     '        Some(JsValue::Number(value)) => *value,',
     '        Some(JsValue::Number(_value)) => default,',
     'timeout_matrix'),
    # 20. fix-r3 P1-1: the shared projection expands a lone surrogate after an
    #     odd run of backslashes, turning an invalid JSON escape into `\\uXXXX`.
    ('surrogate-backslash-reject', text,
     'if backslashes % 2 == 1 {',
     'if backslashes % 2 == 2 {',
     'surrogate_escape_matrix'),
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
    'parse-model-json-utf16': '#771 result planDreamClusters#6',
    'parse-model-json-error-units': '#771 error planDreamClusters#8',
    'markdown-line-terminators': '#771 result rewriteUserProfile#7',
    'timeout-plan-default': '#771 timeout planDreamClusters default 180000ms',
    'timeout-explicit-override': '#771 timeout planDreamClusters override 7ms',
    'surrogate-backslash-reject':
        '#771 surrogate-escape planDreamClusters#9 must be rejected',
}

if '--check-anchors' in sys.argv:
    paths = {path for _name, path, _before, _after, _test in mutations}
    sources = {path: path.read_text() for path in paths}
    bad = [(name, sources[path].count(before), path)
           for name, path, before, _after, _test in mutations
           if sources[path].count(before) != 1]
    for name, count, path in bad:
        print(f'STALE ANCHOR {name}: {count} matches in {path.relative_to(root)}')
    if bad:
        sys.exit(2)
    labels = ', '.join(path.relative_to(root).as_posix() for path in sorted(paths))
    print(f'OK {len(mutations)} anchors in {labels}')
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
               '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_llm_dream',
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
with mutating_sources(root, [dream, text], baseline=baseline) as originals:
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

    baseline_logs = []
    for test in ('wire_matrix', 'timeout_matrix', 'surrogate_escape_matrix'):
        slots, run, log = run_suite(test)
        baseline_logs.append(slots.stdout + slots.stderr + log)
        if run.returncode != 0:
            (out / 'baseline.log').write_text(''.join(baseline_logs))
            print(f'BASELINE FAILED: unmutated {test} must pass before mutations')
            sys.exit(1)
    (out / 'baseline.log').write_text(''.join(baseline_logs))

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

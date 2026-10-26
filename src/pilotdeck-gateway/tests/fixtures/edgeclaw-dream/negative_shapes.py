#!/usr/bin/env python3
"""Reversible mutations for #826 dream malformed-input shape checks.

Each mutant removes exactly one new TS-conformance guard in
``dream/helpers.rs`` and must turn the oracle replay red. The unmutated suite
must pass first; ``could not compile`` is never a kill, and every original byte
is restored in ``finally``. An anchor that no longer occurs exactly once is a
harness defect: print ``STALE ANCHOR`` and exit 2 instead of counting it as
SURVIVED.

``mutating_sources`` requires the gateway product tree to equal the baseline.
This script ships with the fix, so an authoring tree with uncommitted
``helpers.rs`` is refused by design (no shadow git, no second build directory).
The coordinator runs it after committing:

    CARGO_TARGET_DIR=/.../cargo-target \
      python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-dream/negative_shapes.py \
      _tmp/<date>-issue-826/negative-shapes

``NEGATIVE_ONLY=a,b`` runs a sample. cargo always uses the caller's
``CARGO_TARGET_DIR``.
"""
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
fixtures = Path(__file__).resolve().parent
helpers = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/dream/helpers.rs'
out = Path(sys.argv[1]).resolve()
target = os.environ.get('CARGO_TARGET_DIR')
assert target, 'negative_shapes.py requires CARGO_TARGET_DIR from the caller'
baseline = os.environ.get('NEGATIVE_BASELINE', 'HEAD')
out.mkdir(parents=True, exist_ok=True)

# Exact post-fix source spans. Keep them in sync with helpers.rs; a change that
# moves a guard without updating this list fails the stale-anchor check.
clusters_string_block = '''        JsValue::String(_) => {
            return Err(type_error(
                "Cannot read properties of undefined (reading 'map')",
            ));
        }'''
cluster_null_block = '''        JsValue::Null => {
            return Err(type_error(
                "Cannot read properties of null (reading 'memberRelativePaths')",
            ));
        }'''
metas_null_block = '''        Some(JsValue::Null) => {
            return Err(type_error("Cannot read properties of null (reading 'map')"));
        }'''
metas_missing_block = '''        Some(JsValue::Undefined) | None => {
            return Err(type_error(
                "Cannot read properties of undefined (reading 'map')",
            ));
        }'''
meta_null_block = '''            JsValue::Null => {
                return Err(type_error(
                    "Cannot read properties of null (reading 'projectId')",
                ));
            }'''
groups_null_block = '''        Some(JsValue::Null) => {
            return Err(type_error(
                "Cannot read properties of null (reading 'entries')",
            ));
        }'''
groups_missing_block = '''        Some(JsValue::Undefined) | None => {
            return Err(type_error(
                "Cannot read properties of undefined (reading 'entries')",
            ));
        }'''
group_null_block = '''            JsValue::Null => {
                return Err(type_error(
                    "Cannot read properties of null (reading 'keeperProjectId')",
                ));
            }'''
dups_null_block = '''            Some(JsValue::Null) => {
                return Err(type_error("Cannot read properties of null (reading 'map')"));
            }'''
dups_missing_block = '''            Some(JsValue::Undefined) | None => {
                return Err(type_error(
                    "Cannot read properties of undefined (reading 'map')",
                ));
            }'''
drop_reason_block = '''        Some(value) if js_truthy(value) => to_string_units(value)
            .map(JsString::from_utf16)
            .map_err(|_| FileMemoryError::Invalid("dream reason too deep for String()".into())),
        _ => Ok(join_units(members, ", ")),'''

cases = [
    # 1. Non-iterable `clusters` becomes an empty result again.
    ('clusters-not-iterable', helpers,
     '        _ => return Err(type_error("clusters is not iterable")),',
     '        _ => &[],', 'clusters_match_ts'),
    # 2. A non-empty string `clusters` stops iterating characters.
    ('clusters-string', helpers, clusters_string_block,
     '        JsValue::String(_) => &[],', 'clusters_match_ts'),
    # 3. Null cluster entries read as an absent field instead of throwing.
    ('cluster-null-prop', helpers, cluster_null_block,
     '        JsValue::Null => None,', 'clusters_match_ts'),
    # 4. Null memberRelativePaths reads as an empty list.
    ('members-null-map', helpers,
     '        Some(JsValue::Null) => Err(type_error("Cannot read properties of null (reading \'map\')")),',
     '        Some(JsValue::Null) => Ok(&[]),', 'clusters_match_ts'),
    # 5. Missing/undefined memberRelativePaths reads as an empty list.
    ('members-missing-map', helpers,
     '''        Some(JsValue::Undefined) | None => Err(type_error(
            "Cannot read properties of undefined (reading 'map')",
        )),''',
     '        Some(JsValue::Undefined) | None => Ok(&[]),', 'clusters_match_ts'),
    # 6. A non-array memberRelativePaths reads as an empty list.
    ('members-nonarray-map', helpers,
     '''        Some(_) => Err(type_error(
            "cluster.memberRelativePaths.map is not a function",
        )),''',
     '        Some(_) => Ok(&[]),', 'clusters_match_ts'),
    # 7. A non-string member is normalized to "" again.
    ('member-nonstring', helpers,
     '            let member = normalize_whitespace(Some(member))?;',
     '            let member = normalize_whitespace(Some(member)).unwrap_or_default();',
     'clusters_match_ts'),
    # 8. A non-string reason is normalized to "" again.
    ('reason-nonstring', helpers,
     '    let normalized = normalize_whitespace(reason)?;',
     '    let normalized = normalize_whitespace(reason).unwrap_or_default();',
     'clusters_match_ts'),
    # 9. A truthy non-string reason loses its JS String() coercion in a warning.
    ('drop-reason-coerce', helpers, drop_reason_block,
     '        _ => Ok(join_units(members, ", ")),', 'clusters_match_ts'),
    # 10. Non-array `metas` becomes empty again.
    ('metas-nonarray-map', helpers,
     '        Some(_) => return Err(type_error("input.metas.map is not a function")),',
     '        Some(_) => &[],', 'merge_groups_match_ts'),
    # 11. Null `metas` becomes empty again.
    ('metas-null-map', helpers, metas_null_block,
     '        Some(JsValue::Null) => &[],', 'merge_groups_match_ts'),
    # 12. Missing `metas` becomes empty again.
    ('metas-missing-map', helpers, metas_missing_block,
     '        Some(JsValue::Undefined) | None => &[],', 'merge_groups_match_ts'),
    # 13. A null meta entry reads as a missing projectId.
    ('meta-null-entry', helpers, meta_null_block,
     '            JsValue::Null => {}', 'merge_groups_match_ts'),
    # 14. Non-array `groups` becomes empty again.
    ('groups-nonarray-entries', helpers,
     '''        Some(_) => {
            return Err(type_error(
                "input.groups.entries is not a function or its return value is not iterable",
            ));
        }''',
     '        Some(_) => &[],', 'merge_groups_match_ts'),
    # 15. Null `groups` becomes empty again.
    ('groups-null-entries', helpers, groups_null_block,
     '        Some(JsValue::Null) => &[],', 'merge_groups_match_ts'),
    # 16. Missing `groups` becomes empty again.
    ('groups-missing-entries', helpers, groups_missing_block,
     '        Some(JsValue::Undefined) | None => &[],', 'merge_groups_match_ts'),
    # 17. A null group entry reads as a missing keeper.
    ('group-null-keeper', helpers, group_null_block,
     '            JsValue::Null => None,', 'merge_groups_match_ts'),
    # 18. A non-string keeper is normalized to "" again.
    ('keeper-nonstring', helpers,
     '        let keeper = normalize_whitespace(keeper_value)?;',
     '        let keeper = normalize_whitespace(keeper_value).unwrap_or_default();',
     'merge_groups_match_ts'),
    # 19. Null duplicateProjectIds reads as an empty list.
    ('dups-null-map', helpers, dups_null_block,
     '            Some(JsValue::Null) => &[],', 'merge_groups_match_ts'),
    # 20. Missing duplicateProjectIds reads as an empty list.
    ('dups-missing-map', helpers, dups_missing_block,
     '            Some(JsValue::Undefined) | None => &[],', 'merge_groups_match_ts'),
    # 21. A non-array duplicateProjectIds reads as an empty list.
    ('dups-nonarray-map', helpers,
     '''            Some(_) => {
                return Err(type_error(
                    "group.duplicateProjectIds.map is not a function",
                ));
            }''',
     '            Some(_) => &[],', 'merge_groups_match_ts'),
    # 22. A non-string duplicate id is normalized to "" again.
    ('dup-nonstring', helpers,
     '            let value = normalize_whitespace(Some(value))?;',
     '            let value = normalize_whitespace(Some(value)).unwrap_or_default();',
     'merge_groups_match_ts'),
]
env = dict(os.environ)
env['CARGO_TARGET_DIR'] = target
only = {name for name in os.environ.get('NEGATIVE_ONLY', '').split(',') if name}
unknown = only - {case[0] for case in cases}
assert not unknown, f'unknown NEGATIVE_ONLY cases: {sorted(unknown)}'

# Authoring-tree check that does not mutate anything (the committed run still
# goes through `mutating_sources` below).
if os.environ.get('NEGATIVE_CHECK_ANCHORS'):
    source = helpers.read_text()
    stale = [(name, source.count(before)) for name, _path, before, _after, _test in cases
             if source.count(before) != 1]
    for name, count in stale:
        print(f'STALE ANCHOR {name}: {count} matches')
    print(f'anchors: {len(cases) - len(stale)}/{len(cases)} current')
    sys.exit(2 if stale else 0)


def assertion_failure(log):
    if 'could not compile' in log:
        return False
    return 'assertion `left == right` failed' in log or 'expected TypeError' in log


killed, survived = [], []
with mutating_sources(root, [helpers], baseline=baseline) as originals:
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
        print('STALE ANCHORS: negative_shapes.py is out of sync with helpers.rs; refusing to run',
              flush=True)
        sys.exit(2)
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

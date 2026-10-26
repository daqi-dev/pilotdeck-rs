#!/usr/bin/env python3
"""Reversible mutations for #826 dream malformed-input shape checks.

Each mutant withdraws exactly one new TS-conformance guard (or one
error-mapping branch) in ``dream/helpers.rs`` and must turn the oracle replay
red *at the case the mutant claims to cover*. The two nullish branches that
also serve non-object receivers are narrowed to the shape they name (object
with an absent key vs. object with an explicit ``undefined``): a wider mutant
trips an earlier sibling case and cannot prove the named one. The unmutated
suite must pass first; ``could not compile`` is never a kill, and every
original byte is restored. An anchor that no longer occurs exactly once is a
harness defect: print ``STALE ANCHOR`` and exit 2 instead of counting it as
SURVIVED.

Case binding: ``oracle.json`` cases have no id field, so the replay names each
one ``<fn>#<index within that fn>`` in every failure message. The harness below
locates each mutant's target in ``oracle.json`` from its recorded signature and
accepts a kill only when the log contains ``test <test> ... FAILED`` and that
exact ``case=<id>``. A mutant tripped by an earlier sibling case (the P1 from
review-r1), an unrelated panic, or a build failure is not a kill.

Signatures are matched after decoding the fixture's JS-value sentinels:
``{"$undefined": true}`` is a *value*, not an object with no keys. Matching raw
JSON let ``{'duplicateProjectIds': ABSENT}`` falsely bind to a group entry that
is itself ``undefined`` (case #29, which throws at ``keeperProjectId``) instead
of the object that lacks only ``duplicateProjectIds`` (case #36).

``mutating_sources`` requires the gateway product tree to equal the baseline.
This script ships with the fix, so an authoring tree with uncommitted
``helpers.rs`` is refused by design (no shadow git, no second build directory).
The coordinator runs it after committing:

    CARGO_TARGET_DIR=/.../cargo-target \
      python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-dream/negative_shapes.py \
      _tmp/<date>-issue-826/fix-r2/negative-shapes

The uncommitted authoring tree can be named with the dangling commit from
``NEGATIVE_BASELINE=$(git stash create)``; ``NEGATIVE_ONLY=a,b`` runs a sample
and ``NEGATIVE_CHECK_ANCHORS=1`` only checks anchors. Cargo always uses the
caller's ``CARGO_TARGET_DIR``. A surviving mutant is collected, printed, and
exits 1.
"""
import json
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
if not target:
    sys.exit('negative_shapes.py requires CARGO_TARGET_DIR from the caller')
baseline = os.environ.get('NEGATIVE_BASELINE', 'HEAD')
out.mkdir(parents=True, exist_ok=True)

oracle = json.loads((fixtures / 'oracle.json').read_text())

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
drop_reason_checked = '''        Some(value) if js_truthy(value) => to_string_units_checked(value)
            .map(JsString::from_utf16)
            .map_err(|error| match error {
                ToStringError::NotPrimitive => type_error(ToStringError::NOT_PRIMITIVE),
                ToStringError::StackOverflow => {
                    FileMemoryError::Range(StackOverflow::MESSAGE.into())
                }
            }),'''
drop_reason_overflow_map = '''                ToStringError::StackOverflow => {
                    FileMemoryError::Range(StackOverflow::MESSAGE.into())
                }'''

ABSENT = object()
UNDEFINED = object()


def decode(value):
    """Decode the oracle's lossless JS-value sentinels before matching.

    ``{"$undefined": true}`` stands for a JS *value* (``undefined``), not an
    object with no keys; treating it as a plain dict made an ``ABSENT`` key
    signature match a non-object receiver. ``$deepArray`` stays a dict because
    ``deep_reason`` recognizes it without expanding 3000 nested lists.
    ``$number`` is not decoded: no signature currently names a non-finite
    number. The replay decodes the same sentinels before calling the helper.
    """
    if isinstance(value, list):
        return [decode(item) for item in value]
    if isinstance(value, dict):
        if len(value) == 1 and value.get('$undefined') is True:
            return UNDEFINED
        return {key: decode(item) for key, item in value.items()}
    return value


def subset(expected, actual):
    """Match the recorded signature against one case's args[0].

    Dicts are subsets (merge inputs carry four long metas), lists must have the
    same length, ``ABSENT`` requires the key to be missing, and ``UNDEFINED``
    matches the decoded JS ``undefined`` value.
    """
    if isinstance(expected, dict) and isinstance(actual, dict):
        for key, value in expected.items():
            if value is ABSENT:
                if key in actual:
                    return False
            elif key not in actual or not subset(value, actual[key]):
                return False
        return True
    if isinstance(expected, list) and isinstance(actual, list):
        return len(expected) == len(actual) and all(
            subset(item, value) for item, value in zip(expected, actual))
    return expected == actual


def deep_reason(args0):
    try:
        reason = args0[0]['reason']
    except (TypeError, KeyError, IndexError):
        return False
    if isinstance(reason, dict) and isinstance(reason.get('$deepArray'), int):
        return reason['$deepArray'] >= 2000
    depth = 0
    while isinstance(reason, list):
        depth += 1
        reason = reason[0]
    return depth >= 2000


def matches(signature, args0):
    args0 = decode(args0)
    return signature(args0) if callable(signature) else subset(signature, args0)


# (name, before, after, test, fn, target signature, expected thrown name or None)
cases = [
    # 1. Non-iterable `clusters` becomes an empty result again.
    ('clusters-not-iterable',
     '        _ => return Err(type_error("clusters is not iterable")),',
     '        _ => &[],', 'clusters_match_ts', 'validateExclusiveClusters',
     None, 'TypeError'),
    # 2. A non-empty string `clusters` stops iterating characters.
    ('clusters-string', clusters_string_block,
     '        JsValue::String(_) => &[],', 'clusters_match_ts',
     'validateExclusiveClusters', 'ab', 'TypeError'),
    # 3. Null cluster entries read as an absent field instead of throwing.
    ('cluster-null-prop', cluster_null_block,
     '        JsValue::Null => None,', 'clusters_match_ts',
     'validateExclusiveClusters', [None], 'TypeError'),
    # 4. Null memberRelativePaths reads as an empty list.
    ('members-null-map',
     '        Some(JsValue::Null) => Err(type_error("Cannot read properties of null (reading \'map\')")),',
     '        Some(JsValue::Null) => Ok(&[]),', 'clusters_match_ts',
     'validateExclusiveClusters', [{'memberRelativePaths': None}], 'TypeError'),
    # 5. An object with no memberRelativePaths key reads as an empty list.
    #    Non-object receivers also reach the same `None` arm; restricting the
    #    replacement to object clusters keeps case #37 (a number cluster) red
    #    only when its own guard is removed, so the kill lands on #40.
    ('members-missing-map',
     '''        Some(JsValue::Undefined) | None => Err(type_error(
            "Cannot read properties of undefined (reading 'map')",
        )),''',
     '''        Some(JsValue::Undefined) | None if matches!(cluster, JsValue::Object(_)) => Ok(&[]),
        Some(JsValue::Undefined) | None => Err(type_error(
            "Cannot read properties of undefined (reading 'map')",
        )),''', 'clusters_match_ts',
     'validateExclusiveClusters',
     [{'memberRelativePaths': ABSENT}], 'TypeError'),
    # 6. An explicitly undefined memberRelativePaths reads as an empty list.
    #    Only `Some(Undefined)` changes; the absent-key and non-object `None`
    #    path still throws, so the first (and only) observable case is #42.
    ('members-undefined-map',
     '''        Some(JsValue::Undefined) | None => Err(type_error(
            "Cannot read properties of undefined (reading 'map')",
        )),''',
     '''        Some(JsValue::Undefined) => Ok(&[]),
        None => Err(type_error(
            "Cannot read properties of undefined (reading 'map')",
        )),''', 'clusters_match_ts',
     'validateExclusiveClusters',
     [{'memberRelativePaths': UNDEFINED}], 'TypeError'),
    # 7. A non-array memberRelativePaths reads as an empty list.
    ('members-nonarray-map',
     '''        Some(_) => Err(type_error(
            "cluster.memberRelativePaths.map is not a function",
        )),''',
     '        Some(_) => Ok(&[]),', 'clusters_match_ts',
     'validateExclusiveClusters', [{'memberRelativePaths': 1}], 'TypeError'),
    # 8. A non-string member is normalized to "" again.
    ('member-nonstring',
     '            let member = normalize_whitespace(Some(member))?;',
     '            let member = normalize_whitespace(Some(member)).unwrap_or_default();',
     'clusters_match_ts', 'validateExclusiveClusters',
     [{'memberRelativePaths': ['a.md', 1]}], 'TypeError'),
    # 9. A non-string reason is normalized to "" again in the generic-reason guard.
    ('reason-nonstring',
     '    let normalized = normalize_whitespace(reason)?;',
     '    let normalized = normalize_whitespace(reason).unwrap_or_default();',
     'clusters_match_ts', 'validateExclusiveClusters',
     [{'memberRelativePaths': ['a.md', 'b.md'], 'reason': 1}], 'TypeError'),
    # 10. Only the non-string coercion in a dropped warning is removed; string
    #    reasons keep flowing, so the kill must be the numeric one-member case.
    ('drop-reason-coerce', drop_reason_checked,
     '''        Some(JsValue::String(value)) => Ok(value.clone()),
        Some(value) if js_truthy(value) => Ok(join_units(members, ", ")),''',
     'clusters_match_ts', 'validateExclusiveClusters',
     [{'memberRelativePaths': ['a.md'], 'reason': 1}], None),
    # 11. Non-array `metas` becomes empty again.
    ('metas-nonarray-map',
     '        Some(_) => return Err(type_error("input.metas.map is not a function")),',
     '        Some(_) => &[],', 'merge_groups_match_ts',
     'validateGeneralProjectMergeGroups', {'metas': 1}, 'TypeError'),
    # 12. Null `metas` becomes empty again.
    ('metas-null-map', metas_null_block,
     '        Some(JsValue::Null) => &[],', 'merge_groups_match_ts',
     'validateGeneralProjectMergeGroups', {'metas': None}, 'TypeError'),
    # 13. Missing/undefined `metas` becomes empty again.
    ('metas-missing-map', metas_missing_block,
     '        Some(JsValue::Undefined) | None => &[],', 'merge_groups_match_ts',
     'validateGeneralProjectMergeGroups',
     {'metas': ABSENT, 'groups': []}, 'TypeError'),
    # 14. A null meta entry reads as a missing projectId.
    ('meta-null-entry', meta_null_block,
     '            JsValue::Null => {}', 'merge_groups_match_ts',
     'validateGeneralProjectMergeGroups', {'metas': [None]}, 'TypeError'),
    # 15. Non-array `groups` becomes empty again.
    ('groups-nonarray-entries',
     '''        Some(_) => {
            return Err(type_error(
                "input.groups.entries is not a function or its return value is not iterable",
            ));
        }''',
     '        Some(_) => &[],', 'merge_groups_match_ts',
     'validateGeneralProjectMergeGroups', {'groups': 1}, 'TypeError'),
    # 16. Null `groups` becomes empty again.
    ('groups-null-entries', groups_null_block,
     '        Some(JsValue::Null) => &[],', 'merge_groups_match_ts',
     'validateGeneralProjectMergeGroups', {'groups': None}, 'TypeError'),
    # 17. Missing/undefined `groups` becomes empty again.
    ('groups-missing-entries', groups_missing_block,
     '        Some(JsValue::Undefined) | None => &[],', 'merge_groups_match_ts',
     'validateGeneralProjectMergeGroups',
     {'groups': ABSENT}, 'TypeError'),
    # 18. A null group entry reads as a missing keeper.
    ('group-null-keeper', group_null_block,
     '            JsValue::Null => None,', 'merge_groups_match_ts',
     'validateGeneralProjectMergeGroups', {'groups': [None]}, 'TypeError'),
    # 19. A non-string keeper is normalized to "" again.
    ('keeper-nonstring',
     '        let keeper = normalize_whitespace(keeper_value)?;',
     '        let keeper = normalize_whitespace(keeper_value).unwrap_or_default();',
     'merge_groups_match_ts', 'validateGeneralProjectMergeGroups',
     {'groups': [{'keeperProjectId': 1}]}, 'TypeError'),
    # 20. Null duplicateProjectIds reads as an empty list.
    ('dups-null-map', dups_null_block,
     '            Some(JsValue::Null) => &[],', 'merge_groups_match_ts',
     'validateGeneralProjectMergeGroups',
     {'groups': [{'duplicateProjectIds': None}]}, 'TypeError'),
    # 21. An object group with no duplicateProjectIds key reads as an empty
    #     list. Non-object entries also reach the shared `None` arm; restricting
    #     the replacement to object groups keeps case #30 (a number group) red
    #     only when its own guard is removed, so the kill lands on #36.
    ('dups-missing-map', dups_missing_block,
     '''            Some(JsValue::Undefined) | None
                if matches!(group, JsValue::Object(_)) => &[],
            Some(JsValue::Undefined) | None => {
                return Err(type_error(
                    "Cannot read properties of undefined (reading 'map')",
                ));
            }''', 'merge_groups_match_ts',
     'validateGeneralProjectMergeGroups',
     {'groups': [{'duplicateProjectIds': ABSENT}]}, 'TypeError'),
    # 22. An explicitly undefined duplicateProjectIds reads as an empty list.
    #     Only `Some(Undefined)` changes; the absent-key and non-object `None`
    #     path still throws, so the first (and only) observable case is #37.
    ('dups-undefined-map', dups_missing_block,
     '''            Some(JsValue::Undefined) => &[],
            None => {
                return Err(type_error(
                    "Cannot read properties of undefined (reading 'map')",
                ));
            }''', 'merge_groups_match_ts',
     'validateGeneralProjectMergeGroups',
     {'groups': [{'duplicateProjectIds': UNDEFINED}]}, 'TypeError'),
    # 23. A non-array duplicateProjectIds reads as an empty list.
    ('dups-nonarray-map',
     '''            Some(_) => {
                return Err(type_error(
                    "group.duplicateProjectIds.map is not a function",
                ));
            }''',
     '            Some(_) => &[],', 'merge_groups_match_ts',
     'validateGeneralProjectMergeGroups',
     {'groups': [{'duplicateProjectIds': 1}]}, 'TypeError'),
    # 24. A non-string duplicate id is normalized to "" again.
    ('dup-nonstring',
     '            let value = normalize_whitespace(Some(value))?;',
     '            let value = normalize_whitespace(Some(value)).unwrap_or_default();',
     'merge_groups_match_ts', 'validateGeneralProjectMergeGroups',
     {'groups': [{'duplicateProjectIds': ['p2', 1]}]}, 'TypeError'),
    # 25. The unchecked String() swallows the shadowed-toString TypeError.
    ('drop-reason-unchecked', drop_reason_checked,
     '''        Some(value) if js_truthy(value) => crate::js_value::to_string_units(value)
            .map(JsString::from_utf16)
            .map_err(|_| FileMemoryError::Range(StackOverflow::MESSAGE.into())),''',
     'clusters_match_ts', 'validateExclusiveClusters',
     [{'memberRelativePaths': ['a.md'], 'reason': {'toString': None}}], 'TypeError'),
    # 26. A depth overflow is mapped back to the invented Invalid message.
    ('drop-reason-overflow-invalid', drop_reason_overflow_map,
     '''                ToStringError::StackOverflow => {
                    FileMemoryError::Invalid("dream reason too deep for String()".into())
                }''',
     'clusters_match_ts', 'validateExclusiveClusters', deep_reason, 'RangeError'),
]
env = dict(os.environ)
env['CARGO_TARGET_DIR'] = target
only = {name for name in os.environ.get('NEGATIVE_ONLY', '').split(',') if name}
unknown = only - {case[0] for case in cases}
assert not unknown, f'unknown NEGATIVE_ONLY cases: {sorted(unknown)}'

# Resolve each mutant's case id from its recorded signature before touching the
# source. A moved fixture is a stale control, not a surviving mutant.
stale_targets = []
case_ids = {}
for name, _before, _after, _test, fn, signature, throws_name in cases:
    seq = [case for case in oracle['cases'] if case['fn'] == fn]
    found = None
    for index, case in enumerate(seq):
        if not matches(signature, case['args'][0]):
            continue
        if throws_name is None:
            if 'throws' in case:
                continue
        elif case.get('throws', {}).get('name') != throws_name:
            continue
        found = f'{fn}#{index}'
        break
    if found is None:
        stale_targets.append((name, fn, getattr(signature, '__name__', 'signature')))
    else:
        case_ids[name] = found
if stale_targets:
    report = '\n'.join(f'STALE TARGET {name}: no {fn} case matches {kind}'
                       for name, fn, kind in stale_targets) + '\n'
    (out / 'stale-targets.log').write_text(report)
    print(report, end='', flush=True)
    print('STALE TARGETS: negative_shapes.py does not match oracle.json; refusing to run',
          flush=True)
    sys.exit(2)

# Authoring-tree check that does not mutate anything (the committed run still
# goes through `mutating_sources` below).
if os.environ.get('NEGATIVE_CHECK_ANCHORS'):
    source = helpers.read_text()
    stale = [(name, source.count(before)) for name, before, *_rest in cases
             if source.count(before) != 1]
    for name, count in stale:
        print(f'STALE ANCHOR {name}: {count} matches')
    print(f'anchors: {len(cases) - len(stale)}/{len(cases)} current')
    sys.exit(2 if stale else 0)


def assertion_failure(log, test, case_id):
    if 'could not compile' in log:
        return False
    if f'test {test} ... FAILED' not in log:
        return False
    if f'case={case_id}' not in log:
        return False
    return ('assertion `left == right` failed' in log
            or 'expected TypeError' in log
            or 'expected RangeError' in log)


def run_suite(label):
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    cmd = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
           '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_dream_helpers']
    run = run_owned(cmd, cwd=root, env=env, timeout=600)
    (out / f'{label}.log').write_text(slots.stdout + slots.stderr + run.stdout + run.stderr)
    return run.returncode


killed, survived = [], []
with mutating_sources(root, [helpers], baseline=baseline) as originals:
    stale = []
    for name, before, _after, _test, _fn, _signature, _throws in cases:
        count = originals[helpers].decode().count(before)
        if count != 1:
            stale.append((name, helpers.relative_to(root).as_posix(), count))
    if stale:
        report = '\n'.join(
            f'STALE ANCHOR {name}: {count} matches in {relative}'
            for name, relative, count in stale) + '\n'
        (out / 'stale-anchors.log').write_text(report)
        print(report, end='', flush=True)
        print('STALE ANCHORS: negative_shapes.py is out of sync with helpers.rs; refusing to run',
              flush=True)
        sys.exit(2)
    if run_suite('baseline') != 0:
        print('BASELINE FAILED: unmutated suite must pass before mutations', flush=True)
        sys.exit(1)

    for name, before, after, test, _fn, _signature, _throws in cases:
        if only and name not in only:
            continue
        reason = None
        try:
            source = originals[helpers].decode()
            slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
            helpers.write_text(source.replace(before, after))
            try:
                cmd = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
                       '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_dream_helpers',
                       test, '--', '--exact']
                run = run_owned(cmd, cwd=root, env=env, timeout=300)
                log = run.stdout + run.stderr
                (out / f'{name}.log').write_text(slots.stdout + slots.stderr + log)
                if not (run.returncode != 0
                        and assertion_failure(log, test, case_ids[name])):
                    reason = f'did not fail case={case_ids[name]} in {test}'
            finally:
                helpers.write_bytes(originals[helpers])
        except Exception as error:
            reason = f'{type(error).__name__}: {error}'
        if reason is None:
            killed.append(name)
            print(f'KILLED   {name}: {test} case={case_ids[name]}', flush=True)
        else:
            survived.append((name, reason))
            print(f'SURVIVED {name}: {reason}', flush=True)
    helpers.write_bytes(originals[helpers])
    assert helpers.read_bytes() == originals[helpers], 'restoration failed'
print(f'KILLED   {len(killed)}: ' + ', '.join(killed))
print(f'SURVIVED {len(survived)}: ' + ', '.join(name for name, _ in survived))
# The restore path must pass too: a mutant that cannot be returned to the
# committed bytes would poison the next run.
if run_suite('restored') != 0:
    print('RESTORED FAILED: source did not return to a passing baseline', flush=True)
    sys.exit(1)
if survived:
    sys.exit(1)

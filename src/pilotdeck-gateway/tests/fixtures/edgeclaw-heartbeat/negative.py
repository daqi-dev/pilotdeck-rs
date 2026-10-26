#!/usr/bin/env python3
"""Reversible product mutations for edgeclaw-heartbeat.

Every case compiles and must fail a runtime oracle assertion; a compile error
never counts as a kill. The script runs the unmutated suite first, then every
mutant, prints KILLED/SURVIVED and exits 1 when any mutant survives. The
original bytes are restored in ``finally`` by ``mutating_sources``.

New product files are not in HEAD before the coordinator commits them, so this
script is normally run after that commit. For a pre-commit sample, point
``NEGATIVE_BASELINE`` at a dangling commit object that contains the authoring
tree (see RESULT.md). ``NEGATIVE_ONLY=a,b`` narrows the run for a sample.

cargo always uses the caller's ``CARGO_TARGET_DIR``; without it the script
exits before writing any file.
"""
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
fixtures = Path(__file__).resolve().parent
helpers = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/heartbeat/helpers.rs'
out = Path(sys.argv[1]).resolve()
target = os.environ.get('CARGO_TARGET_DIR')
assert target, 'negative.py requires CARGO_TARGET_DIR from the caller'
baseline = os.environ.get('NEGATIVE_BASELINE', 'HEAD')
out.mkdir(parents=True, exist_ok=True)

oracle_groups = {case['group'] for case in
                 json.loads((fixtures / 'oracle.json').read_text())['cases']}
assertion_anchors = sorted(oracle_groups)

cases = [
    # 1. sameMessage ignores role; two roles with equal content collapse.
    ('same-role', 'left.role == right.role && left.content == right.content',
     'left.content == right.content', 'messages/sameMessage'),
    # 2. Equal-length identical transcripts are reported as new content.
    ('new-content-length', 'if incoming.len() > previous.len() {',
     'if incoming.len() >= previous.len() {', 'messages/hasNewContent'),
    # 3. A shared prefix is always treated as a replacement, even at 0.
    ('merge-prefix-threshold', 'if prefix_length > 0 {',
     'if prefix_length >= 0 {', 'merge/mergeSessionMessages'),
    # 4. A shared prefix reports the whole incoming transcript as new.
    ('merge-new-tail', 'new_messages: incoming[prefix_length..].to_vec(),',
     'new_messages: incoming.to_vec(),', 'merge/mergeSessionMessages'),
    # 5. Flatten forgets the seed transcript.
    ('flatten-seed', 'let mut previous = seed_messages.to_vec();',
     'let mut previous = Vec::new();', 'flatten/flattenBatchMessages'),
    # 6. Focus turns keep assistant messages.
    ('focus-assistant', '.filter(|message| message.role == MemoryRole::User)',
     '.filter(|message| message.role != MemoryRole::User)', 'focus/deriveFocusTurns'),
    # 7. Three-unit CJK grams disappear.
    ('token-three-gram', 'for size in [2, 3] {', 'for size in [2] {',
     'token/tokenizeSearchText'),
    # 8. The whole Han run is kept only up to 7 units.
    ('token-han-whole', 'if length <= 8 {', 'if length <= 7 {',
     'token/tokenizeSearchText'),
    # 9. "项目" is no longer a stopword.
    ('token-stopword', '"项目",', '"项目x",', 'token/tokenizeSearchText'),
    # 10. Exact project-name matches score half as much.
    ('shortlist-exact-weight', ')) * 2;', ')) * 1;',
     'shortlist/buildGeneralProjectShortlist'),
    # 11. Tie-break sorting runs ascending instead of descending.
    ('shortlist-tie-order',
     'node_compare_utf16(shortlist_collator(), &right.updated_at, &left.updated_at)',
     'node_compare_utf16(shortlist_collator(), &left.updated_at, &right.updated_at)',
     'shortlist/buildGeneralProjectShortlist'),
    # 12. The shortlist cuts one entry too early.
    ('shortlist-limit', '.take(GENERAL_INDEX_PROJECT_CANDIDATE_LIMIT)',
     '.take(GENERAL_INDEX_PROJECT_CANDIDATE_LIMIT - 1)',
     'shortlist/buildGeneralProjectShortlist'),
    # 13. The routing query drops the stage field.
    ('routing-stage',
     'for key in ["name", "description", "rule", "summary", "stage"] {',
     'for key in ["name", "description", "rule", "summary"] {',
     'routing/buildCandidateRoutingQuery'),
    # 14. The preview body header is misspelled.
    ('preview-body-header', 'lines.push(JsString::from("## Body"));',
     'lines.push(JsString::from("## Bodies"));',
     'preview/buildCandidateMemoryPreview'),
    # 15. The preview keeps 2999 units instead of 3000.
    ('preview-truncate', 'preview.units()[..3000].to_vec()',
     'preview.units()[..2999].to_vec()', 'preview/buildCandidateMemoryPreview'),
    # 16. previewText cuts one unit early at the exact boundary.
    ('preview-max', 'if normalized.len_utf16() <= max_chars {',
     'if normalized.len_utf16() < max_chars {', 'trace/previewText'),
    # 17. User-note detection loses its directory suffix.
    ('storage-user-note', '&"UserNotes/".encode_utf16()',
     '&"UserNote/".encode_utf16()', 'trace/inferStorageKind'),
    # 18. A null/undefined kv value renders as "[object Object]" instead of "".
    ('kv-null-value', 'None | Some(JsValue::Undefined) | Some(JsValue::Null) => Vec::new(),',
     'None | Some(JsValue::Undefined) | Some(JsValue::Null) => "[object Object]".encode_utf16().collect::<Vec<_>>(),',
     'detail/kvDetail'),
    # 19. Step ids start at 2.
    ('step-id-offset', 'steps.len() + 1', 'steps.len() + 2', 'step/createStep'),
    # 20. createStep keeps falsy optional values (null/undefined).
    ('step-options-truthy', '            && truthy(value)\n', '',
     'step/createStep'),
    # 21. Empty timestamps stay in the batch summary.
    ('batch-empty-timestamp', '.filter(|timestamp| !timestamp.is_empty())\n', '',
     'batch/createBatchTrace'),
    # 22. Batch timestamps are not sorted.
    ('batch-timestamp-sort',
     '''    timestamps.sort_by(|left, right| {
        left.encode_utf16()
            .collect::<Vec<_>>()
            .cmp(&right.encode_utf16().collect::<Vec<_>>())
    });''',
     '', 'batch/createBatchTrace'),
]
env = dict(os.environ)
env['CARGO_TARGET_DIR'] = target
only = {name for name in os.environ.get('NEGATIVE_ONLY', '').split(',') if name}
unknown = only - {case[0] for case in cases}
assert not unknown, f'unknown NEGATIVE_ONLY cases: {sorted(unknown)}'


def assertion_failure(log):
    if 'could not compile' in log:
        return False
    if 'test result: FAILED.' not in log:
        return False
    return any(f'{anchor}/' in log for anchor in assertion_anchors)


killed, survived = [], []
with mutating_sources(root, [helpers], baseline=baseline) as originals:
    base_cmd = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
                '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_heartbeat']
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    run = run_owned(base_cmd, cwd=root, env=env, timeout=600)
    (out / 'baseline.log').write_text(slots.stdout + slots.stderr + run.stdout + run.stderr)
    if run.returncode != 0:
        print('BASELINE FAILED: unmutated suite must pass before mutations', flush=True)
        sys.exit(1)

    for name, before, after, anchor in cases:
        if only and name not in only:
            continue
        reason = None
        try:
            source = originals[helpers].decode()
            assert source.count(before) == 1, (name, source.count(before))
            slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
            helpers.write_text(source.replace(before, after))
            try:
                cmd = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
                       '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_heartbeat',
                       'heartbeat_helpers_match_ts', '--', '--exact']
                run = run_owned(cmd, cwd=root, env=env, timeout=300)
                log = run.stdout + run.stderr
                (out / f'{name}.log').write_text(slots.stdout + slots.stderr + log)
                if not (run.returncode != 0 and assertion_failure(log)):
                    reason = 'did not fail a narrowed assertion'
                elif anchor not in log:
                    reason = f'failed without the {anchor} oracle anchor'
            finally:
                helpers.write_bytes(originals[helpers])
        except Exception as error:
            reason = f'{type(error).__name__}: {error}'
        if reason is None:
            killed.append(name)
            print(f'KILLED   {name}: {anchor}', flush=True)
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

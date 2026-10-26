#!/usr/bin/env python3
"""Reversible product mutations for edgeclaw-heartbeat.

Every case compiles and must fail a runtime oracle assertion; a compile error
never counts as a kill. The script runs the unmutated suite first, then every
mutant, and one final unmutated suite after restoration; it prints
KILLED/SURVIVED and exits 1 when any mutant survives. The original bytes are
restored in ``finally`` by ``mutating_sources``.

New product files are not in HEAD before the coordinator commits them, so this
script is normally run after that commit. ``NEGATIVE_ONLY=a,b`` narrows the run
for a sample. cargo always uses the caller's ``CARGO_TARGET_DIR``; without it
the script exits before writing any file.

Every mutation of ``heartbeat/helpers.rs`` stays in the single-source ``cases``
table that main's ``anchor_selfcheck.py`` recognizes for this path (IMPLICIT
``helpers``). The three mutants for the shared helpers
(``js_value`` / ``storage`` / ``text``) live in ``negative_shared.py`` and are
imported here, so one run still covers all 36 cases.

Cases cover every branch changed by #852:

* P1-1 role preservation (drop non-user/assistant roles again);
* P1-2 checked ``String(value)`` conversion (unchecked fallback), the shared
  depth bound (2001 containers) and the no-panic contract (panic on a
  conversion error);
* P3 shared helpers (``storage::js_truthy`` and ``text::join_text_refs`` are
  actually used by the heartbeat code).

#831's eight dedicated mutants (``commonPrefixLength``, ``normalizeTrigger``,
``buildIndexTraceId``, ``exposeStoredRelativePath`` and the text/note/list/json
detail constructors) are merged as cases 29-36. The trace-id and global-prefix
anchors are rewritten for #852's shared ``text::join_text_refs`` helper with
the same semantics. Cases 10-12 (shared ``js_value`` / ``storage`` / ``text``
files) are defined in ``negative_shared.py``.
"""
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

sys.path.insert(0, str(Path(__file__).resolve().parent))
from negative_shared import js_value, mutations as shared_mutations, storage, text

root = Path(__file__).resolve().parents[5]
helpers = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/heartbeat/helpers.rs'
paths = {
    'helpers': helpers,
    'text': text,
    'storage': storage,
    'js_value': js_value,
}
out = Path(sys.argv[1]).resolve()
target = os.environ.get('CARGO_TARGET_DIR')
assert target, 'negative.py requires CARGO_TARGET_DIR from the caller'
baseline = os.environ.get('NEGATIVE_BASELINE', 'HEAD')
out.mkdir(parents=True, exist_ok=True)

GENERIC = 'heartbeat_helpers_match_ts'
DEPTH = 'kv_detail_depth_error_is_reported_not_panicking'

# (name, before, after, test, specific failure anchor). Main's anchor selfcheck
# resolves the implicit single source (helpers) and reads before/after here.
cases = [
    # -- P1-1: role preservation ---------------------------------------------
    # 1. sameMessage ignores role; two roles with equal content collapse.
    ('same-role',
     'left.role == right.role && left.content == right.content',
     'left.content == right.content',
     GENERIC, 'messages/sameMessage'),
    # 2. Non-user/assistant roles are dropped again (the original bug). The
    # first dropped message is the system role in messages/sameMessage, so the
    # helper's role-specific panic is the declared anchor (a generic
    # expect("message") would hide which role and case were lost).
    ('messages-drop-other-roles',
     'let role = MemoryRole::from_js(role);',
     'let role = MemoryRole::from_js(role);\n'
     '            if matches!(role, MemoryRole::Other(_)) {\n'
     '                return None;\n'
     '            }',
     GENERIC, 'heartbeat message dropped: role=system'),
    # 3. Equal-length identical transcripts are reported as new content.
    ('new-content-length',
     'if incoming.len() > previous.len() {',
     'if incoming.len() >= previous.len() {',
     GENERIC, 'messages/hasNewContent'),
    # 4. A shared prefix is always treated as a replacement, even at 0.
    ('merge-prefix-threshold',
     'if prefix_length > 0 {',
     'if prefix_length >= 0 {',
     GENERIC, 'merge/mergeSessionMessages'),
    # 5. A shared prefix reports the whole incoming transcript as new.
    ('merge-new-tail',
     'new_messages: incoming[prefix_length..].to_vec(),',
     'new_messages: incoming.to_vec(),',
     GENERIC, 'merge/mergeSessionMessages'),
    # 6. Flatten forgets the seed transcript.
    ('flatten-seed',
     'let mut previous = seed_messages.to_vec();',
     'let mut previous = Vec::new();',
     GENERIC, 'flatten/flattenBatchMessages'),
    # 7. Focus turns keep assistant messages.
    ('focus-assistant',
     '.filter(|message| message.role == MemoryRole::User)',
     '.filter(|message| message.role != MemoryRole::User)',
     GENERIC, 'focus/deriveFocusTurns'),
    # -- P1-2: checked String(value) and no panic ----------------------------
    # 8. The checked conversion falls back to unchecked (own toString).
    ('kv-checked-conversion',
     '                Some(value) => crate::js_value::to_string_units_checked(value)\n'
     '                    .map_err(KvValueError::from_conversion)?,',
     '                Some(value) => crate::js_value::to_string_units(value)\n'
     '                    .map_err(|_| KvValueError::StackOverflow)?,',
     GENERIC, 'detail/kvDetailCatching'),
    # 9. The conversion panics instead of returning an error (P0 contract).
    ('kv-no-panic',
     '    fn from_conversion(error: crate::js_value::ToStringError) -> Self {\n'
     '        match error {\n'
     '            crate::js_value::ToStringError::StackOverflow => Self::StackOverflow,\n'
     '            crate::js_value::ToStringError::NotPrimitive => Self::NotPrimitive,\n'
     '        }\n'
     '    }',
     '    fn from_conversion(_error: crate::js_value::ToStringError) -> Self {\n'
     '        panic!("String(value) does not overflow");\n'
     '    }',
     GENERIC, 'String(value) does not overflow'),
    # -- existing coverage unrelated to #852 ----------------------------------
    # 13. Three-unit CJK grams disappear.
    ('token-three-gram', 'for size in [2, 3] {', 'for size in [2] {',
     GENERIC, 'token/tokenizeSearchText'),
    # 14. The whole Han run is kept only up to 7 units.
    ('token-han-whole', 'if length <= 8 {', 'if length <= 7 {',
     GENERIC, 'token/tokenizeSearchText'),
    # 15. "项目" is no longer a stopword.
    ('token-stopword', '"项目",', '"项目x",',
     GENERIC, 'token/tokenizeSearchText'),
    # 16. Exact project-name matches score half as much.
    ('shortlist-exact-weight', ')) * 2;', ')) * 1;',
     GENERIC, 'shortlist/buildGeneralProjectShortlist'),
    # 17. Tie-break sorting runs ascending instead of descending.
    ('shortlist-tie-order',
     'node_compare_utf16(shortlist_collator(), &right.updated_at, &left.updated_at)',
     'node_compare_utf16(shortlist_collator(), &left.updated_at, &right.updated_at)',
     GENERIC, 'shortlist/buildGeneralProjectShortlist'),
    # 18. The shortlist cuts one entry too early.
    ('shortlist-limit', '.take(GENERAL_INDEX_PROJECT_CANDIDATE_LIMIT)',
     '.take(GENERAL_INDEX_PROJECT_CANDIDATE_LIMIT - 1)',
     GENERIC, 'shortlist/buildGeneralProjectShortlist'),
    # 19. The routing query drops the stage field.
    ('routing-stage',
     'for key in ["name", "description", "rule", "summary", "stage"] {',
     'for key in ["name", "description", "rule", "summary"] {',
     GENERIC, 'routing/buildCandidateRoutingQuery'),
    # 20. The preview body header is misspelled.
    ('preview-body-header', 'lines.push(JsString::from("## Body"));',
     'lines.push(JsString::from("## Bodies"));',
     GENERIC, 'preview/buildCandidateMemoryPreview'),
    # 21. The preview keeps 2999 units instead of 3000.
    ('preview-truncate', 'preview.units()[..3000].to_vec()',
     'preview.units()[..2999].to_vec()',
     GENERIC, 'preview/buildCandidateMemoryPreview'),
    # 22. previewText cuts one unit early at the exact boundary.
    ('preview-max', 'if normalized.len_utf16() <= max_chars {',
     'if normalized.len_utf16() < max_chars {',
     GENERIC, 'trace/previewText'),
    # 23. User-note detection loses its directory suffix.
    ('storage-user-note', '&"UserNotes/".encode_utf16()',
     '&"UserNote/".encode_utf16()',
     GENERIC, 'trace/inferStorageKind'),
    # 24. A null/undefined kv value renders as "[object Object]" instead of "".
    ('kv-null-value',
     'None | Some(JsValue::Undefined) | Some(JsValue::Null) => Vec::new(),',
     'None | Some(JsValue::Undefined) | Some(JsValue::Null) => '
     '"[object Object]".encode_utf16().collect::<Vec<_>>(),',
     GENERIC, 'detail/kvDetail'),
    # 25. Step ids start at 2.
    ('step-id-offset', 'steps.len() + 1', 'steps.len() + 2',
     GENERIC, 'step/createStep'),
    # 26. createStep keeps falsy optional values (null/undefined).
    ('step-options-truthy', '            && js_truthy(value)\n', '',
     GENERIC, 'step/createStep'),
    # 27. Empty timestamps stay in the batch summary.
    ('batch-empty-timestamp', '.filter(|timestamp| !timestamp.is_empty())\n', '',
     GENERIC, 'batch/createBatchTrace'),
    # 28. Batch timestamps are not sorted.
    ('batch-timestamp-sort',
     '    timestamps.sort_by(|left, right| {\n'
     '        left.encode_utf16()\n'
     '            .collect::<Vec<_>>()\n'
     '            .cmp(&right.encode_utf16().collect::<Vec<_>>())\n'
     '    });',
     '',
     GENERIC, 'batch/createBatchTrace'),
    # -- #831: dedicated mutants for the remaining pure functions -------------
    # 29. commonPrefixLength stops one message before the shared prefix ends.
    ('common-prefix-off-by-one',
     'while index < limit && same_message(previous.get(index), incoming.get(index)) {',
     'while index + 1 < limit && same_message(previous.get(index), incoming.get(index)) {',
     GENERIC, 'merge/commonPrefixLength'),
    # 30. normalizeTrigger stops folding the reason to lowercase.
    ('normalize-trigger-case',
     'let normalized = text::lowercase(&text::trim_js(reason.unwrap_or(&JsString::default())));',
     'let normalized = text::trim_js(reason.unwrap_or(&JsString::default()));',
     GENERIC, 'trace/normalizeTrigger'),
    # 31. buildIndexTraceId joins the l0 ids with ';' instead of ','.
    ('trace-id-separator',
     'let joined = text::join_text_refs(l0_ids.iter(), ",");',
     'let joined = text::join_text_refs(l0_ids.iter(), ";");',
     GENERIC, 'trace/buildIndexTraceId'),
    # 32. Global relative paths lose the "global/" directory prefix.
    ('expose-global-prefix',
     'text::join_text_refs([&JsString::from("global/"), relative_path], "")',
     'text::join_text_refs([&JsString::from("global"), relative_path], "")',
     GENERIC, 'trace/exposeStoredRelativePath'),
    # 33. textDetail returns the raw escaped text instead of decoding it.
    ('detail-text-decode',
     '''    detail(
        "text",
        key,
        label,
        label_i18n,
        vec![(
            "text",
            JsValue::String(text::decode_escaped_unicode_text(text, true)),
        )],
    )''',
     '''    detail(
        "text",
        key,
        label,
        label_i18n,
        vec![(
            "text",
            JsValue::String(text.clone()),
        )],
    )''',
     GENERIC, 'detail/textDetail'),
    # 34. noteDetail returns the raw escaped text instead of decoding it.
    ('detail-note-decode',
     '''    detail(
        "note",
        key,
        label,
        label_i18n,
        vec![(
            "text",
            JsValue::String(text::decode_escaped_unicode_text(text, true)),
        )],
    )''',
     '''    detail(
        "note",
        key,
        label,
        label_i18n,
        vec![(
            "text",
            JsValue::String(text.clone()),
        )],
    )''',
     GENERIC, 'detail/noteDetail'),
    # 35. listDetail returns raw escaped items instead of decoding them.
    ('detail-list-decode',
     '        .map(|item| JsValue::String(text::decode_escaped_unicode_text(item, true)))',
     '        .map(|item| JsValue::String(item.clone()))',
     GENERIC, 'detail/listDetail'),
    # 36. jsonDetail returns the raw JSON value without decoding strings.
    ('detail-json-decode',
     'vec![("json", text::decode_escaped_unicode_value(json, true))],',
     'vec![("json", json.clone())],',
     GENERIC, 'detail/jsonDetail'),
]
# Shared-helper rows come from negative_shared.py. They use the generic
# multi-source table (name, path, before, after, test, anchor) that the same
# checker supports; both tables stay in one mutating_sources lock.
rows = [(name, [(helpers, before, after)], test, anchor)
        for name, before, after, test, anchor in cases]
rows.extend((name, [(path, before, after)], test, anchor)
            for name, path, before, after, test, anchor in shared_mutations)
env = dict(os.environ)
env['CARGO_TARGET_DIR'] = target
only = {name for name in os.environ.get('NEGATIVE_ONLY', '').split(',') if name}
unknown = only - {row[0] for row in rows}
assert not unknown, f'unknown NEGATIVE_ONLY cases: {sorted(unknown)}'


def assertion_failure(log, anchor):
    if 'could not compile' in log:
        return False
    if 'test result: FAILED.' not in log:
        return False
    return anchor in log


def cargo_test(test):
    command = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
               '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_heartbeat']
    if test:
        command.extend([test, '--', '--exact'])
    return run_owned(command, cwd=root, env=env, timeout=600)


class AnchorExpired(RuntimeError):
    """A mutation's anchor no longer matches exactly once.

    This is a script bug, not a surviving mutant: abort instead of recording
    SURVIVED, otherwise a stale anchor would look like a weak test.
    """


killed, survived = [], []
with mutating_sources(root, [paths[key] for key in paths], baseline=baseline) as originals:
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    run = cargo_test(None)
    (out / 'baseline.log').write_text(slots.stdout + slots.stderr + run.stdout + run.stderr)
    if run.returncode != 0:
        print('BASELINE FAILED: unmutated suite must pass before mutations', flush=True)
        sys.exit(1)

    for name, edits, test, anchor in rows:
        if only and name not in only:
            continue
        reason = None
        try:
            for key, before, after in edits:
                path = paths[key] if isinstance(key, str) else key
                source = path.read_text()
                count = source.count(before)
                if count != 1:
                    raise AnchorExpired(
                        f'{name}: {path.name} anchor matched {count} times (expected 1); '
                        f'update negative.py before running')
                path.write_text(source.replace(before, after))
            slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
            run = cargo_test(test)
            log = run.stdout + run.stderr
            (out / f'{name}.log').write_text(slots.stdout + slots.stderr + log)
            if not (run.returncode != 0 and assertion_failure(log, anchor)):
                reason = 'did not fail the narrowed assertion'
            elif anchor not in log:
                reason = f'failed without the {anchor} anchor'
        except AnchorExpired as error:
            for path, data in originals.items():
                path.write_bytes(data)
            print(f'ANCHOR EXPIRED: {error}', flush=True)
            sys.exit(2)
        except Exception as error:
            reason = f'{type(error).__name__}: {error}'
        finally:
            for path, data in originals.items():
                path.write_bytes(data)
        if reason is None:
            killed.append(name)
            print(f'KILLED   {name}: {anchor}', flush=True)
        else:
            survived.append((name, reason))
            print(f'SURVIVED {name}: {reason}', flush=True)
    for path, data in originals.items():
        path.write_bytes(data)
        assert path.read_bytes() == data, 'restoration failed: ' + str(path)
    # #831: the mutation loop restores after every case; run the suite once
    # more under the same tree lock so the green-after-restore half of the
    # red/green pair has its own raw log and no other runner can mutate in
    # between.
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    run = cargo_test(None)
    restored_log = slots.stdout + slots.stderr + run.stdout + run.stderr
    (out / 'restored.log').write_text(restored_log)
    if run.returncode != 0 or 'test result: ok.' not in restored_log:
        print('RESTORED FAILED: the suite does not pass after restoring the original bytes',
              flush=True)
        sys.exit(1)
print(f'KILLED   {len(killed)}: ' + ', '.join(killed))
print(f'SURVIVED {len(survived)}: ' + ', '.join(name for name, _ in survived))
if survived:
    sys.exit(1)

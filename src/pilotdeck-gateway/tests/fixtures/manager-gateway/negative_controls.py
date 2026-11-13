#!/usr/bin/env python3
"""U4 manager product mutations; each must turn a named test red.

The product files are new in this package, so `mutating_sources` refuses until
the branch is committed (the source baseline must equal HEAD). After the
coordinator commits:

    CARGO_TARGET_DIR=<caller-owned cargo target directory> \
    PDRS_BASELINE=/path/to/PilotDeck \
      python3 src/pilotdeck-gateway/tests/fixtures/manager-gateway/negative_controls.py \
      <out-dir> [--only name,...] [name ...]

`--audit` only checks every anchor against the current source (under the shared
tree lock — a busy lock or an unreadable source is a refusal, not a verdict);
it does not mutate, compile, or require a commit.

The entry follows the #1666 / #1734 self-run contract on the shared
`negative_selection` helpers: the table is pinned to the static
`EXPECTED_NAMES` roster (drift refuses to start, rc 2, no evidence directory
claimed), an unknown or repeated selection name and an unusable or
already-claimed output directory / target refuse the same way, and the verdict
roster is re-checked *before* anything that reads as success is published: a
run whose recorded verdicts do not match the roster writes
`roster-mismatch.json` instead of `summary.json` and exits 1, so no green
summary or green count line ever exists for a shrunken run (#1666, the
memory-gateway / snapshot-projection r5/r6 ordering). The published summary
records the run's selection (`selected`, the executed set in table order), so
a subset run's green artifact is auditable for what ran and what did not. The
loop is owned here, not delegated to `negative_suite.run_cases`, because that
shared runner publishes its summary before control returns to the caller.
The window is self-held for hours, so SIGTERM/SIGHUP/SIGQUIT unwind through
the restore path instead of leaving a mutant in the tree, and every write to
a shared product source is one atomic rename (never a truncating in-place
write).
"""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys

root = Path(__file__).resolve().parents[5]
fixtures = root / 'src/pilotdeck-gateway/tests/fixtures'
sys.path[:0] = [str(fixtures), str(fixtures / 'turn-runner')]
# negative_process/negative_suite own mutating_sources/run_owned/wait_for_slot
# and the judging/publishing helpers; no local backup/restore or alternate
# baseline is permitted here.
import pending_reanchor as pending
from negative_process import (defer_signal_exit, mutating_sources, run_owned,
                              tree_lock, wait_for_slot, write_source_atomic)
from negative_selection import (claim_dir, missing_target,
                                 refuse_unusable_target, registered, select,
                                 verdict_exit)
from negative_suite import (Case, case_labels, classify, invalidate_completed,
                            passed, _publish_json)
from _negative_common import mutate

catalog = root / 'src/pilotdeck-gateway/src/runtime/manager/catalog.rs'
file_catalog = root / 'src/pilotdeck-gateway/src/runtime/manager/file_catalog.rs'
projection = root / 'src/pilotdeck-gateway/src/runtime/manager/projection.rs'
metadata_path = root / 'src/pilotdeck-gateway/src/metadata.rs'
dispatch = root / 'src/pilotdeck-gateway/src/dispatch.rs'
adapter = root / 'src/pilotdeck-gateway/src/storage/transcript_adapter.rs'
js_value_path = root / 'src/pilotdeck-gateway/src/js_value.rs'
session_model = root / 'src/pilotdeck-gateway/src/session_model.rs'
skills_path = root / 'src/pilotdeck-gateway/src/skills/mod.rs'
binding = root / 'src/pilotdeck-gateway/src/config/binding.rs'
prefix = 'runtime::manager::tests::'

# (name, source, original, mutant, scope, bound test, exclusive failure text)
# Every mutation is applied to the pristine original, never cumulatively.
cases = [
    ('catalog-tail-partial-dropped', file_catalog, '''    Some(SessionLite {
        mtime,''', '''    if !tail.ends_with(b"\\n") {
        return None;
    }
    Some(SessionLite {
        mtime,''', None,
     prefix + 'manager_catalog_keeps_committed_prefix_when_tail_is_half_written',
     '#934 a half-written tail must keep the committed session'),
    ('catalog-internal-filter-removed', file_catalog,
     '        if !include_internal && is_internal_session(session_id) {',
     '        if false && !include_internal && is_internal_session(session_id) {', None,
     prefix + 'manager_catalog_filters_internal_before_reading',
     '#934 default manager list must hide always-on sessions before reading'),
    ('catalog-cwd-removed', file_catalog, '''    info.insert(
        "cwd".into(),
        JsValue::from(project_root.to_string_lossy().as_ref()),
    );
''', '', None,
     prefix + 'manager_catalog_matrix_matches_real_ts',
     '#934 catalog matrix differs from real TS at committed'),
    ('catalog-summary-trimmed', file_catalog,
     '''    let Some(summary) = custom_title.clone().or(ai_title.clone()).or(last_prompt) else {
        return Ok(None);
    };''',
     '''    let Some(summary) = custom_title.clone().or(ai_title.clone()).or(last_prompt) else {
        return Ok(None);
    };
    let summary = JsString::from_utf16(js_trim_units(summary.units()).to_vec());''', None,
     prefix + 'manager_catalog_matrix_matches_real_ts',
     '#934 catalog matrix differs from real TS at metadata-title-spaces'),
    ('invalid-params-fallback', projection,
     '            JsValue::Null => Some("null"),',
     '            JsValue::Null => None,', None,
     prefix + 'manager_sessions_invalid_params_match_ts',
     '#934 invalid manager params must fail'),
    ('invalid-project-key-stringified', projection, '''            other => {
                // Reuse the gateway's Node `util.inspect`/`ToString` wording.
                // Node evaluates `'name' in constructor` and
                // `${constructor.name}` while describing the argument, so a
                // failure there escapes as its own TypeError/RangeError before
                // `ERR_INVALID_ARG_TYPE` is ever constructed.
                let received = match metadata::argument_received(other) {
                    metadata::ArgumentReceived::Text(received) => received,
                    metadata::ArgumentReceived::Throws { name, message } => {
                        return Err(ManagerError::with_message(name, None, message));
                    }
                };
                return Err(ManagerError::with_message(
                    "TypeError",
                    Some("ERR_INVALID_ARG_TYPE".into()),
                    metadata::invalid_arg_type_message("paths[0]", &received),
                ));
            }''', '''            other => return Err(ManagerError::plain("TypeError", format!("{other:?}"))),''', None,
     prefix + 'manager_sessions_invalid_params_match_ts',
     # The stringified TypeError drops the Node code first, before the message check.
     '#934 invalid projectKey must keep the TS ERR_INVALID_ARG_TYPE code'),
    ('pagination-negative-end', adapter, '''    if value < 0.0 {
        (len as f64 + value).max(0.0) as usize
    } else {
        value.min(len as f64) as usize
    }''', '''    value.max(0.0).min(len as f64) as usize''', None,
     prefix + 'manager_catalog_paginates_like_js_slice',
     '#934 limit=-1 must slice from the end like JS'),
    ('browser-port-always-unavailable', projection, '''    pub fn list(&self, input: &JsValue) -> Result<JsValue, ManagerError> {
        self.provider
            .as_ref()
            .ok_or_else(|| {
                ManagerError::gateway(
                    "CAPABILITY_UNAVAILABLE",
                    "Gateway manager browser provider is unavailable.",
                )
            })?
            .list(input)
    }''', '''    pub fn list(&self, input: &JsValue) -> Result<JsValue, ManagerError> {
        let _ = (&self.provider, input);
        Err(ManagerError::gateway(
            "CAPABILITY_UNAVAILABLE",
            "Gateway manager browser provider is unavailable.",
        ))
    }''', None,
     prefix + 'manager_browsers_uses_injected_provider',
     '#934 browser provider success path must return its items'),
    ('error-code-projection', projection,
     'out.insert("code".into(), code.as_str().into());',
     'out.insert("code".into(), "gateway_request_failed".into());', None,
     prefix + 'manager_error_projection_matches_ts',
     '#934 provider error code must be preserved'),
    ('session-key-filter-removed', projection,
     '        let items = if truthy(session_key) {',
     '        let items = if false && truthy(session_key) {', None,
     prefix + 'manager_sessions_matches_real_ts_projection',
     '#934 manager gateway oracle replay differs at sessions-filter-id'),
    ('catalog-limit', catalog,
     '            limit: Some(MANAGER_SESSION_LIMIT),',
     '            limit: Some(50.0),', None,
     prefix + 'manager_catalog_consumer_requests_ts_limit',
     '#934 manager catalog must request TS limit 100'),
    ('dispatch-registration', dispatch,
     '    "native_archive_artifact",\n];',
     '    "native_archive_artifact",\n    "manager_sessions",\n];', None,
     prefix + 'manager_methods_stay_outside_default_dispatch',
     '#934 manager_sessions must not join LOCAL_METHODS in this package'),
    ('unknown-project-lossy', projection,
     '            message: JsString::from_utf16(message),',
     '            message: JsString::from(String::from_utf16_lossy(&message)),', None,
     prefix + 'manager_projects_keeps_unknown_key_units',
     '#934 unknown key message must keep raw UTF-16 units'),
    ('mtime-node-rounding', file_catalog,
     '    metadata::node_date_ms(metadata::millis(time))',
     '    { let ms = metadata::millis(time); ms as u64 as f64 }', None,
     prefix + 'manager_catalog_mtime_matches_node_millisecond_rounding',
     '#934 sub-millisecond mtime must round like Node getTime'),
    ('same-mtime-name-order', file_catalog,
     '    entries.sort_by_key(|entry| entry.file_name());',
     '    entries.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));', None,
     prefix + 'manager_catalog_same_mtime_orders_by_name',
     '#934 same-mtime sessions must keep Node readdir name order'),
    ('surrogate-fast-title-lossy', metadata_path,
     '        Ok(value) => Ok(value.as_string().cloned()),',
     '        Ok(value) => Ok(value.as_string().map(|text| JsString::from(String::from_utf16_lossy(text.units())))),', None,
     prefix + 'manager_catalog_preserves_lone_surrogates',
     '#934 fast metadata title must keep lone surrogate units'),
    ('surrogate-prompt-lossy', metadata_path,
     '                        result = Some(JsString::from_utf16(trimmed.to_vec()));',
     '                        result = Some(JsString::from(String::from_utf16_lossy(trimmed)));', None,
     prefix + 'manager_catalog_preserves_lone_surrogates',
     '#934 prompt text must keep lone surrogate units'),
    ('surrogate-fallback-lossy', file_catalog,
     '    let string = |key: &str| metadata.get(key).and_then(JsValue::as_string).cloned();',
     '    let string = |key: &str| metadata.get(key).and_then(JsValue::as_string).map(|text| JsString::from(String::from_utf16_lossy(text.units())));', None,
     prefix + 'manager_catalog_preserves_lone_surrogates',
     '#934 metadata fallback title must keep lone surrogate units'),
    ('metadata-title-escape-swallowed', file_catalog,
     '        if let Some(captured) = metadata::captured_field(line, field)? {',
     '        if let Ok(Some(captured)) = metadata::captured_field(line, field) {', None,
     prefix + 'manager_catalog_propagates_metadata_syntax_errors',
     '#934 invalid metadata title escape must reject like TS SyntaxError'),
    ('metadata-createdat-escape-swallowed', file_catalog,
     '    match metadata::captured_field(head, "createdAt")? {',
     '    match metadata::captured_field(head, "createdAt").unwrap_or(None) {', None,
     prefix + 'manager_catalog_propagates_metadata_syntax_errors',
     '#934 invalid createdAt escape must reject like TS SyntaxError'),
    ('projectkey-name-explained-by-object', metadata_path,
     '                            text.extend_from_slice(&units);',
     '                            text.extend_from_slice("Object".encode_utf16().collect::<Vec<_>>().as_slice());', None,
     prefix + 'manager_sessions_matches_real_ts_projection',
     '#934 manager gateway oracle replay differs at sessions-projectKey-name-object'),
    ('projectkey-name-array-branch', metadata_path, '''                    let name = constructor.get("name").unwrap_or(&undefined);
                    // Reuse the shared iterative ToString: its MAX_ARRAY_DEPTH''', '''                    let name = constructor.get("name").unwrap_or(&undefined);
                    if matches!(name, JsValue::Array(_)) {
                        return Text("[object Array]".into());
                    }
                    // Reuse the shared iterative ToString: its MAX_ARRAY_DEPTH''', None,
     prefix + 'manager_sessions_matches_real_ts_projection',
     '#934 manager gateway oracle replay differs at sessions-projectKey-name-array'),
    ('projectkey-name-surrogate-lossy', js_value_path,
     '        JsValue::String(s) => out.extend_from_slice(s.units()),',
     '''        JsValue::String(s) => out.extend_from_slice(
            String::from_utf16_lossy(s.units())
                .encode_utf16()
                .collect::<Vec<_>>()
                .as_slice(),
        ),''', None,
     prefix + 'manager_sessions_matches_real_ts_projection',
     '#934 manager gateway oracle replay differs at sessions-projectKey-name-surrogate'),
    ('internal-filter-after-read', file_catalog, '''        if !include_internal && is_internal_session(session_id) {
            continue;
        }
        let path = entry.path();
        if let Some((modified, info)) =
            read_session_info(&path, &JsString::from(session_id), project_root)?
        {
            sessions.push((modified, info));
        }''', '''        let path = entry.path();
        let loaded = read_session_info(&path, &JsString::from(session_id), project_root);
        if !include_internal && is_internal_session(session_id) {
            continue;
        }
        if let Ok(Some((modified, info))) = loaded {
            sessions.push((modified, info));
        }''', None,
     prefix + 'manager_catalog_filters_internal_before_reading',
     '#934 internal sessions must be filtered before any file read'),


    ('deep-project-key-recursive-tostring', js_value_path, '''            JsValue::Array(items) => {
                if depth >= MAX_ARRAY_DEPTH {
                    return Err(ToStringError::StackOverflow);
                }
                pending.extend(items.iter().rev().map(|item| (item, depth + 1)));
            }''', '''            JsValue::Array(items) => {
                fn depth_of(value: &JsValue) -> usize {
                    match value {
                        JsValue::Array(items) => 1 + items.iter().map(depth_of).max().unwrap_or(0),
                        _ => 0,
                    }
                }
                if depth_of(value) >= MAX_ARRAY_DEPTH {
                    return Err(ToStringError::StackOverflow);
                }
                pending.extend(items.iter().rev().map(|item| (item, depth + 1)));
            }''', None,
     prefix + 'manager_deep_project_key_returns_range_error_without_aborting',
     '#934 deep projectKey child must not abort'),
    ('projectkey-description-throws-rewritten', projection, '''                let received = match metadata::argument_received(other) {
                    metadata::ArgumentReceived::Text(received) => received,
                    metadata::ArgumentReceived::Throws { name, message } => {
                        return Err(ManagerError::with_message(name, None, message));
                    }
                };''', '''                let received = match metadata::argument_received(other) {
                    metadata::ArgumentReceived::Text(received) => received,
                    metadata::ArgumentReceived::Throws { .. } => "[Object]".into(),
                };''', None,
     prefix + 'manager_sessions_invalid_params_match_ts',
     '#934 invalid projectKey must keep the TS ERR_INVALID_ARG_TYPE code'),
    ('shared-fast-title-trimmed', metadata_path, '''        .or_else(|| accepted_text(&head, false))
        .is_some_and(|text| !text.is_empty());''', '''        .or_else(|| accepted_text(&head, false))
        .is_some_and(|text| !js_trim_units(text.units()).is_empty());''', None,
     'metadata::tests::whitespace_only_title_keeps_shared_metadata_session_count',
     '#934 whitespace-only title must count as a shared metadata summary'),
    ('metadata-only-lite-field-order', file_catalog, '''        SessionInfoOrder::Metadata => {
            info.insert("createdAt".into(), created_at);
            info.insert("tag".into(), string(tag));
        }''', '''        SessionInfoOrder::Metadata => {
            info.insert("tag".into(), string(tag));
            info.insert("createdAt".into(), created_at);
        }''', None,
     prefix + 'manager_catalog_metadata_only_keeps_ts_field_order',
     '#934 metadata-only fallback must keep the TS createdAt-before-tag order'),
    ('shared-descriptor-session-model-lossy', session_model, '''            crate::metadata::invalid_arg_type_message("paths[0]", &received),''', '''            crate::metadata::invalid_arg_type_message(
                "paths[0]",
                &JsString::from(String::from_utf16_lossy(received.units())),
            ),''', None,
     'session_model::tests::path_argument_preserves_lone_surrogate_units',
     '#934 session_model path argument must keep the lone surrogate unit'),
    ('shared-descriptor-skills-lossy', skills_path, '''                crate::metadata::invalid_arg_type_message("paths[0]", &received),''', '''                crate::metadata::invalid_arg_type_message(
                    "paths[0]",
                    &JsString::from(String::from_utf16_lossy(received.units())),
                ),''', None,
     'skills::tests::path_argument_preserves_lone_surrogate_units',
     '#934 skills path argument must keep the lone surrogate unit'),
    ('shared-descriptor-binding-lossy', binding, '''        crate::metadata::invalid_arg_type_message("path", received),''', '''        crate::metadata::invalid_arg_type_message(
            "path",
            &pilotdeck_protocol::JsString::from(String::from_utf16_lossy(received.units())),
        ),''', None,
     'config::tests::binding_path_argument_preserves_lone_surrogate_units',
     '#934 binding path argument must keep the lone surrogate unit'),
    ('shared-descriptor-manager-lossy', projection, '''                    metadata::invalid_arg_type_message("paths[0]", &received),''', '''                    metadata::invalid_arg_type_message(
                        "paths[0]",
                        &JsString::from(String::from_utf16_lossy(received.units())),
                    ),''', None,
     prefix + 'manager_sessions_matches_real_ts_projection',
     '#934 manager gateway oracle replay differs at sessions-projectKey-name-surrogate'),
    ('skills-throws-host-flag-dropped', skills_path,
     '            crate::metadata::ArgumentReceived::Throws { .. } => Err(SkillError::host_required())',
     '''            crate::metadata::ArgumentReceived::Throws { .. } => {
                Err(SkillError::host_failure("JS_HOST_REQUIRED", ""))
            }''', None,
     'skills::tests::throws_description_project_key_requires_the_host',
     '#970 a throwing description-phase projectKey must require the host: {"projectKey":{"constructor":5}}'),
    ('metadata-full-title-untrimmed', file_catalog,
     '''    if js_trim_units(summary.units()).is_empty() {
        return Ok(None);
    }''',
     '''    if summary.is_empty() {
        return Ok(None);
    }''', None,
     prefix + 'manager_catalog_blank_title_paths_match_real_ts',
     '#970 a blank title outside both windows must hide the session like TS'),
    ('shared-metadata-full-title-untrimmed', metadata_path,
     '''        .and_then(summary_string)
        .is_some_and(|text| !js_trim_units(text.units()).is_empty())
}''',
     '''        .and_then(summary_string)
        .is_some_and(|text| !text.is_empty())
}''', None,
     'metadata::tests::blank_title_full_scan_keeps_the_ts_trim_rule',
     '#970 a blank title outside both windows must not count as a summary'),
    # #2309: a dispatch failure is `Error`, never a flattened response body
    # (the snapshot-projection family's `error-mapped-to-response` row,
    # mirrored).
    ('error-mapped-to-response', projection,
     '            Err(error) => ManagerDispatch::Error(error),\n',
     '            Err(error) => ManagerDispatch::Response(error.as_json()),\n', None,
     prefix + 'dispatch_routes_only_manager_methods',
     '#2309 manager_sessions failure must dispatch as ManagerDispatch::Error'),
]

# #2309 fix-r1/fix-r2 (MM boundary P1, DS boundary P2, then the two-family
# concurrency P1): the denominator of this run is this static roster, not
# `len(cases)`. A deleted or renamed row would silently shrink the run and
# keep printing a green count (#1666). `main` refuses to start on drift and
# re-checks the recorded verdicts after the run but *before* publishing
# anything that reads as success, the same contract as the memory-gateway /
# snapshot-projection families.
EXPECTED_NAMES = (
    'catalog-tail-partial-dropped',
    'catalog-internal-filter-removed',
    'catalog-cwd-removed',
    'catalog-summary-trimmed',
    'invalid-params-fallback',
    'invalid-project-key-stringified',
    'pagination-negative-end',
    'browser-port-always-unavailable',
    'error-code-projection',
    'session-key-filter-removed',
    'catalog-limit',
    'dispatch-registration',
    'unknown-project-lossy',
    'mtime-node-rounding',
    'same-mtime-name-order',
    'surrogate-fast-title-lossy',
    'surrogate-prompt-lossy',
    'surrogate-fallback-lossy',
    'metadata-title-escape-swallowed',
    'metadata-createdat-escape-swallowed',
    'projectkey-name-explained-by-object',
    'projectkey-name-array-branch',
    'projectkey-name-surrogate-lossy',
    'internal-filter-after-read',
    'deep-project-key-recursive-tostring',
    'projectkey-description-throws-rewritten',
    'shared-fast-title-trimmed',
    'metadata-only-lite-field-order',
    'shared-descriptor-session-model-lossy',
    'shared-descriptor-skills-lossy',
    'shared-descriptor-binding-lossy',
    'shared-descriptor-manager-lossy',
    'skills-throws-host-flag-dropped',
    'metadata-full-title-untrimmed',
    'shared-metadata-full-title-untrimmed',
    'error-mapped-to-response',
)

def terminate_cleanly(signum, _frame):
    """Unwind on SIGTERM/SIGHUP/SIGQUIT so the finally blocks restore the
    sources; the default disposition would leave a mutant in the tree
    (#2309 fix-r3, mirror of mode-routing's handler)."""
    if defer_signal_exit(signum):
        return
    raise SystemExit(128 + signum)


def install_terminate_handlers():
    signal.signal(signal.SIGTERM, terminate_cleanly)
    if hasattr(signal, 'SIGHUP'):
        signal.signal(signal.SIGHUP, terminate_cleanly)
    if hasattr(signal, 'SIGQUIT'):
        signal.signal(signal.SIGQUIT, terminate_cleanly)


def main():
    args = sys.argv[1:]
    only = None
    if '--only' in args:
        index = args.index('--only')
        if index + 1 >= len(args):
            print('--only needs a comma-separated mutation name list', file=sys.stderr)
            return 2
        only = args[index + 1].split(',')
        del args[index:index + 2]
    audit = '--audit' in args
    args = [arg for arg in args if arg != '--audit']
    if not args and not audit:
        print('usage: negative_controls.py <output-dir> [--only name,...] [name ...]'
              ' | --audit', file=sys.stderr)
        return 2
    requested = args[1:] + (only or [])
    # #2309 fix-r1: a drifted roster is a "never started" (2) and refuses
    # before the evidence directory is claimed, so a shrunken table leaves no
    # out/ behind — never a quietly shorter green run (#1666).
    try:
        registered(cases, EXPECTED_NAMES)
    except ValueError as error:
        print(f'mutation roster drifted: {error}', file=sys.stderr)
        return 2
    # #2309 fix-r2 (DS concurrency/boundary P2a, MM concurrency P2): an
    # unknown or repeated selection name is the same "never started" (2) as
    # roster drift. The old inline set() filter folded both silently, so a
    # mistyped `--only` name ran a shorter green scope over a claimed evidence
    # directory (#1734). `select` is the one shared rule, and it runs before
    # the directory is claimed.
    try:
        selected = select(cases, requested, always=())
    except ValueError as error:
        print(f'invalid mutation selection: {error}', file=sys.stderr)
        return 2
    selected_names = set(requested)
    if audit:
        # #2795 fix-r2（评审 sibling P2②，snapshot-projection --audit 的 fix-r1 条2
        # 同族跟改）：锚点核对也要持树锁读——兄弟变异运行器在自己的 write 与 restore
        # 之间可能把变异体留在盘上，无锁读会把锚点核对错在它不拥有的源上；锁忙 /
        # 读失败是拒跑（2），不是宿主会读成 rc 1 的裸 traceback（issue-2495 的
        # 60 s 有界等待先例）。
        try:
            with tree_lock(root, timeout=60):
                for name, path, old, new, scope, _test, _expected in selected:
                    mutate(path.read_text(), scope, [(old, new)])
        except (TimeoutError, OSError, UnicodeDecodeError) as error:
            print(f'--audit tree lock or read failed: '
                  f'{type(error).__name__}: {error}', file=sys.stderr)
            return 2
        print(json.dumps([
            {'mutation': name, 'test': test, 'panic': expected}
            for name, _path, _old, _new, _scope, test, expected in selected
        ], indent=2))
        return 0

    # The family contract, not a local copy: absent/empty/blank targets and
    # the dot/root/src values are refused by the shared rule, and an already
    # existing directory is a "never started" (2) rather than a bare traceback
    # that reads as "it ran and did not come out green" (#1666 fix-r7/r9,
    # #1734). A `SystemExit` carrying a *string* exits 1, so every refusal
    # names the cause on stderr and returns an explicit int 2.
    if missing_target():
        print('CARGO_TARGET_DIR must be set by the caller', file=sys.stderr)
        return 2
    refuse_unusable_target()
    if not (os.environ.get('PDRS_BASELINE') or '').strip():
        print('PDRS_BASELINE must be set by the caller', file=sys.stderr)
        return 2
    # #2309 fix-r3 (MM concurrency P2): the shared registry's identity key is
    # this entry's own path, never `sys.argv[0]` — a wrapper or symlink launch
    # resolves argv[0] outside the fixtures tree and `for_script` raises
    # `ValueError`. That call used to sit inside the guarded window, where the
    # except arm published a 36-row all-INVALID summary that reads as "it ran
    # and did not pass"; reading the registry is a startup precondition, so it
    # runs before the evidence directory is claimed and a refusal is rc 2
    # with no out/ behind (the http-store-park / negative_suite.matrix_main
    # shape of an explicit `script=`).
    try:
        registrations = pending.for_script(root, Path(__file__).resolve())
    except ValueError as error:
        print(f'pending-reanchor registry is not usable: {error}', file=sys.stderr)
        return 2
    out = claim_dir(args[0])
    # The guarded window below is self-held for hours; a termination signal
    # must unwind through the restore path, not kill the process over a
    # mutant source (#2309 fix-r3, the family's mode-routing precedent).
    install_terminate_handlers()
    env = os.environ.copy()
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only',
               CARGO_PROFILE_TEST_DEBUG='line-tables-only', CARGO_TERM_QUIET='false')
    selected_cases = [Case(name, path, old, new, test, expected, scope)
                      for name, path, old, new, scope, test, expected in selected]
    labels = case_labels(selected_cases)
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--lib']
    results = []

    def stem(case):
        return labels[id(case)]

    def run(label, test):
        try:
            slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
            (out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
            # Exact binding still runs one test, including explicit TS/regeneration
            # guards. No `--nocapture`: it interleaves test stdout with the
            # `test <name> ... ok` status line (#938 r14). No `-q`: quiet mode
            # drops the named per-test line the judge needs (#273, #818 fix-r3).
            result = run_owned(command + [test, '--', '--exact', '--include-ignored'],
                               cwd=root, env=env, timeout=900)
            (out / f'{label}.log').write_text(result.stdout + result.stderr)
            return result
        except Exception as error:
            raw = ''
            if isinstance(error, subprocess.TimeoutExpired):
                for value in (error.stdout, error.stderr):
                    raw += value.decode(errors='replace') if isinstance(value, bytes) else value or ''
            with (out / f'{label}.log').open('a') as log:
                log.write(raw + f'\n{type(error).__name__}: {error}\n')
            raise

    def record(case, status, reason):
        # Memory only: the summary is published after the guarded restore /
        # re-verify window has closed *and* the roster check below has passed
        # (#1076, fix-r2).
        results.append({'mutation': case.mutation, 'test': case.test,
                        'assertion': case.assertion, 'status': status, 'reason': reason})
        print(f'{stem(case)}: {status}: {reason}', flush=True)

    def publish_run_summary():
        # #2309 fix-r3 (DS/MM boundary P2): the summary records this run's
        # selection, so a subset run's green artifact is auditable for what
        # ran and what did not -- `selected` is the executed set in table
        # order, and a full run lists every roster name. The payload is the
        # shared publish_summary shape plus that one key; negative_suite is
        # M6-owned (#1099), so the enriched summary rides the same atomic
        # `_publish_json` publication here instead of widening the shared
        # helper.
        _publish_json(out / 'summary.json',
                      {'controls': results,
                       'counts': pending.counts(results),
                       'selected': [case.mutation for case in selected_cases]},
                      overwrite=True)

    # Where the guarded window raised, for the three-state exit contract:
    # 'enter' is a refusal to run (HEAD/baseline mismatch, busy lock) and keeps
    # the historical rc=1; 'restore'/'exit' are unverified restores and must
    # exit 2; 'body' is an in-window precondition such as an expired anchor,
    # also a refusal (#1076 fix-r1). ``body_error`` remembers the object the
    # guarded body raised; a guard that raises while unwinding raises a
    # *different* object even though ``phase`` still says 'body', which is an
    # unverified restore and must exit 2 (#1076 fix-r2).
    phase = 'enter'
    body_error = None
    try:
        paths = list(dict.fromkeys(case.path for case in selected_cases))
        if registrations:
            paths.append(root / pending.FIXTURES / pending.REGISTRY)
        with mutating_sources(root, paths, baseline='HEAD') as originals:
            phase = 'body'
            try:
                for case in selected_cases:
                    entry = pending.match(registrations, case.mutation)
                    if entry is not None:
                        record(case, pending.SKIPPED, entry.reason())
                        continue
                    status, reason, stage = 'INVALID', '', 'anchor'
                    try:
                        changed = mutate(originals[case.path].decode(), case.scope,
                                         [(case.old, case.new)])
                        stage = 'positive'
                        if not passed(run(stem(case) + '-positive', case.test), case.test):
                            reason = 'positive control did not pass the bound test'
                        else:
                            stage = 'negative'
                            # One atomic rename, never a truncating in-place
                            # write: a SIGKILL in the write-back window must
                            # not leave a half-written source behind (#1903,
                            # the mode-routing precedent).
                            write_source_atomic(root, case.path, changed.encode())
                            status, reason = classify(run(stem(case) + '-negative', case.test), case)
                    except Exception as error:
                        status, reason = 'INVALID', f'{stage}: {type(error).__name__}: {error}'
                    finally:
                        # Restore before every successor, also on timeout or bad
                        # anchors. The enclosing guard independently restores
                        # and verifies on exit. Atomic for the same #1903
                        # reason as the mutant write above: the restore window
                        # must not leave a truncated original behind either.
                        phase = 'restore'
                        write_source_atomic(root, case.path, originals[case.path])
                        if case.path.read_bytes() != originals[case.path]:
                            raise RuntimeError(f'byte restoration failed: {case.path}')
                        phase = 'body'
                    try:
                        if not passed(run(stem(case) + '-restored', case.test), case.test):
                            status, reason = 'INVALID', reason + '; restored bound test did not pass'
                    except Exception as error:
                        status, reason = 'INVALID', reason + f'; restored: {type(error).__name__}: {error}'
                    record(case, status, reason)
            except BaseException as error:
                # Keep the refusal/cancellation object so the handler below
                # can tell it apart from a failure raised by the guard exit.
                body_error = error
                raise
            # The body is done; an exception from here on comes from the
            # guard's own restore/re-verify on context exit.
            phase = 'exit'
    except BaseException as error:
        # A baseline/lock/restoration failure — or a cancellation delivered by
        # the guard's restore/re-verify window — makes every recorded row
        # untrustworthy. Void them in memory, account for unrun cases, publish
        # the all-INVALID summary, and exit per the #1076 contract. This is the
        # only summary published on a failure path and it never reads as
        # success; no final summary exists before this point, so an interrupt
        # during the publish leaves no summary at all (missing is fail-closed).
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for case in selected_cases[len(results):]:
            record(case, 'INVALID', reason)
        publish_run_summary()
        # A raw SystemExit code must not decide the process status: 256/-256
        # truncate to zero on POSIX and a string code exits 1. Re-raise the
        # normalized code; a KeyboardInterrupt returns 2.
        if isinstance(error, SystemExit):
            raise SystemExit(2) from error
        if (phase in ('restore', 'exit') or not isinstance(error, Exception)
                or (body_error is not None and error is not body_error)):
            return 2
        return 1
    # Reaching here means mutating_sources restored every source and verified
    # it against the baseline. The roster check runs *before* anything that
    # reads as success is published: a shrunken run used to leave an all-green
    # summary.json and a green count line behind (run_cases published and
    # printed them, then this entry unlinked after the fact), so a consumer
    # that only reads the artifact never saw the mismatch (r5 concurrency P1,
    # fix-r2). A mismatch publishes no summary.json at all — only a file whose
    # name says what happened (r6 concurrency/boundary P1), atomically: the
    # shared `_publish_json` temp name carries the pid and a serial, never a
    # fixed `<name>.tmp` (#1579 r5).
    expected = [name for name in EXPECTED_NAMES if not selected_names or name in selected_names]
    recorded = [row['mutation'] for row in results]
    if recorded != expected:
        message = f'verdict roster mismatch: recorded={recorded} expected={expected}'
        print(message, file=sys.stderr)
        _publish_json(out / 'roster-mismatch.json',
                      {'verdict': 'ROSTER_MISMATCH', 'expected': expected,
                       'recorded': recorded, 'rows': results},
                      overwrite=True)
        return 1
    publish_run_summary()
    counts = pending.counts(results)
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
    # Every row must be KILLED and at least one row must exist: an empty run or
    # a zero-kill run is never success (#818 fix-r3 P0).
    return verdict_exit(results, key='status')


if __name__ == '__main__':
    raise SystemExit(main())

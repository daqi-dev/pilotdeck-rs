#!/usr/bin/env python3
"""U4 manager product mutations; each must turn a named test red.

The product files are new in this package, so `mutating_sources` refuses until
the branch is committed (the source baseline must equal HEAD). After the
coordinator commits:

    CARGO_TARGET_DIR=<worktree>/_build/cargo-target \
    PDRS_BASELINE=/path/to/PilotDeck \
      python3 src/pilotdeck-gateway/tests/fixtures/manager-gateway/negative_controls.py <out-dir>

`--audit` only checks every anchor against the current source; it does not
mutate, compile, or require a commit.
"""
import json
import os
from pathlib import Path
import sys

root = Path(__file__).resolve().parents[5]
fixtures = root / 'src/pilotdeck-gateway/tests/fixtures'
sys.path[:0] = [str(fixtures), str(fixtures / 'turn-runner')]
# negative_suite owns mutating_sources/run_owned/wait_for_slot; no local
# backup/restore or alternate baseline is permitted here.
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, run_cases
from _negative_common import mutate

catalog = root / 'src/pilotdeck-gateway/src/runtime/manager/catalog.rs'
file_catalog = root / 'src/pilotdeck-gateway/src/runtime/manager/file_catalog.rs'
projection = root / 'src/pilotdeck-gateway/src/runtime/manager/projection.rs'
metadata_path = root / 'src/pilotdeck-gateway/src/metadata.rs'
dispatch = root / 'src/pilotdeck-gateway/src/dispatch.rs'
adapter = root / 'src/pilotdeck-gateway/src/storage/transcript_adapter.rs'
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
                // The description is UTF-16 so a lone surrogate in
                // `constructor.name` reaches the wire unchanged; `[Object]` is
                // the `inspect(value, { depth: -1 })` fallback for the exotic
                // shapes where Node throws a different TypeError first.
                let received = metadata::argument_received(other)
                    .unwrap_or_else(|| JsString::from("[Object]"));
                let mut message = "The \\"paths[0]\\" argument must be of type string. Received "
                    .encode_utf16()
                    .collect::<Vec<_>>();
                message.extend_from_slice(received.units());
                return Err(ManagerError::with_message(
                    "TypeError",
                    Some("ERR_INVALID_ARG_TYPE".into()),
                    JsString::from_utf16(message),
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
     '                    let text = js_to_string(name)?;',
     '                    let text = JsString::from("Object");', None,
     prefix + 'manager_sessions_matches_real_ts_projection',
     '#934 manager gateway oracle replay differs at sessions-projectKey-name-object'),
    ('projectkey-name-array-branch', metadata_path, '''                if !matches!(item, JsValue::Null | JsValue::Undefined) {
                    units.extend_from_slice(js_to_string(item)?.units());
                }
            }
            JsString::from_utf16(units)''', '''                if !matches!(item, JsValue::Null | JsValue::Undefined) {
                    units.extend_from_slice(js_to_string(item)?.units());
                }
            }
            JsString::from("[object Array]")''', None,
     prefix + 'manager_sessions_matches_real_ts_projection',
     '#934 manager gateway oracle replay differs at sessions-projectKey-name-array'),
    ('projectkey-name-surrogate-lossy', metadata_path,
     '        JsValue::String(text) => text.clone(),',
     '''        JsValue::String(text) => {
            JsString::from(String::from_utf16_lossy(text.units()))
        }''', None,
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

]

def main():
    if '--audit' in sys.argv[1:]:
        for name, path, old, new, scope, _test, _expected in cases:
            mutate(path.read_text(), scope, [(old, new)])
        print(json.dumps([
            {'mutation': name, 'test': test, 'panic': expected}
            for name, _path, _old, _new, _scope, test, expected in cases
        ], indent=2))
        return 0

    env = os.environ.copy()
    if not env.get('CARGO_TARGET_DIR') or not env.get('PDRS_BASELINE'):
        raise SystemExit('caller must set CARGO_TARGET_DIR and PDRS_BASELINE')
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only',
               CARGO_PROFILE_TEST_DEBUG='line-tables-only', CARGO_TERM_QUIET='false')
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=False)
    return run_cases(
        root,
        [Case(name, path, old, new, test, expected, scope)
         for name, path, old, new, scope, test, expected in cases],
        out, env, mutate, timeout=900, baseline='HEAD')


if __name__ == '__main__':
    raise SystemExit(main())

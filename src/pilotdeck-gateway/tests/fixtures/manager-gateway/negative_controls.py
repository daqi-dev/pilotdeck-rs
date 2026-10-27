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
projection = root / 'src/pilotdeck-gateway/src/runtime/manager/projection.rs'
dispatch = root / 'src/pilotdeck-gateway/src/dispatch.rs'
prefix = 'runtime::manager::tests::'

# (name, source, original, mutant, scope, bound test, exclusive failure text)
# Every mutation is applied to the pristine original, never cumulatively.
cases = [
    ('catalog-consumer-direct-transcript', catalog, '''        let sessions = sessions
            .into_iter()
            .filter(|session| input.include_internal || !is_internal_session(session))
            .collect();
        Ok(paginate(sessions, input.offset.unwrap_or(0.0), input.limit))''', '''        Ok(sessions)''', None,
     prefix + 'manager_sessions_uses_catalog_consumer_not_direct_transcript',
     '#934 catalog consumer must filter internal sessions before pagination'),
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
    ('catalog-unavailable-empty', catalog,
     '            None => return Err(ManagerError::catalog_unavailable()),',
     '            None => return Ok(Vec::new()),', None,
     prefix + 'manager_catalog_reports_unavailable_store',
     '#934 a store without list must not become an empty catalog'),
]

if '--audit' in sys.argv[1:]:
    for name, path, old, new, scope, _test, _expected in cases:
        mutate(path.read_text(), scope, [(old, new)])
    print(json.dumps([
        {'mutation': name, 'test': test, 'panic': expected}
        for name, _path, _old, _new, _scope, test, expected in cases
    ], indent=2))
    raise SystemExit()

env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR') or not env.get('PDRS_BASELINE'):
    raise SystemExit('caller must set CARGO_TARGET_DIR and PDRS_BASELINE')
env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only',
           CARGO_PROFILE_TEST_DEBUG='line-tables-only', CARGO_TERM_QUIET='false')
out = Path(sys.argv[1])
out.mkdir(parents=True, exist_ok=False)
raise SystemExit(run_cases(
    root,
    [Case(name, path, old, new, test, expected, scope)
     for name, path, old, new, scope, test, expected in cases],
    out, env, mutate, timeout=900, baseline='HEAD'))

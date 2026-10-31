#!/usr/bin/env python3
"""#1154 archive cwd controls: --check; --compile-only OUT; OUT [names]."""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
# matrix_main uses these shared guarded primitives for every mutation.
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import matrix_main

ROOT = Path(__file__).resolve().parents[5]
REQUESTS = ROOT / 'src/pilotdeck-gateway/src/native_process/requests.rs'
NATIVE_ARCHIVE = ROOT / 'src/pilotdeck-gateway/src/native_archive.rs'
REGRESSION = ROOT / 'src/pilotdeck-gateway/src/native_process/metadata_cwd_tests.rs'
LEDGER = ROOT / 'specs/gateway-parity/diffs/A7.2-NATIVE-ARCHIVE-CWD-001.md'
ORACLE = Path(__file__).resolve().parent / 'oracle.json'
PREFIX = 'native_process::tests::metadata_cwd::'
MUTATIONS = [
    ('manifest-no-fallback', REQUESTS,
     'Ok("native_archive_manifest"', 'Ok("disabled_archive_manifest"',
     'archive_matrix_matches_ts', '#1154 archive configured fallback and explicit key boundary'),
    ('entries-no-fallback', REQUESTS,
     '| "native_archive_entries"', '| "disabled_archive_entries"',
     'archive_matrix_matches_ts', '#1154 archive configured fallback and explicit key boundary'),
    ('artifact-no-fallback', REQUESTS,
     '| "native_archive_artifact")', '| "disabled_archive_artifact")',
     'archive_matrix_matches_ts', '#1154 archive configured fallback and explicit key boundary'),
    ('null-not-defaulted', NATIVE_ARCHIVE,
     'None | Some(JsValue::Undefined | JsValue::Null)', 'None | Some(JsValue::Undefined)',
     'archive_matrix_matches_ts', '#1154 archive configured fallback and explicit key boundary'),
    ('missing-not-defaulted', NATIVE_ARCHIVE,
     'None | Some(JsValue::Undefined | JsValue::Null)', 'Some(JsValue::Undefined | JsValue::Null)',
     'archive_matrix_matches_ts', '#1154 archive configured fallback and explicit key boundary'),
    ('fallback-process-cwd', NATIVE_ARCHIVE,
     'fallback.to_string_lossy().as_ref().into(),',
     'std::env::current_dir().unwrap_or_default().to_string_lossy().as_ref().into(),',
     'archive_matrix_matches_ts', '#1154 archive configured fallback and explicit key boundary'),
    ('empty-string-defaulted', NATIVE_ARCHIVE,
     'None | Some(JsValue::Undefined | JsValue::Null)',
     'None | Some(JsValue::Undefined | JsValue::Null | JsValue::String(_))',
     'archive_matrix_matches_ts', '#1154 archive configured fallback and explicit key boundary'),
    ('isolated-admission-skipped', REQUESTS,
     'if let Some(project) = params.get("projectKey").filter(|v| !v.is_undefined())',
     'if let Some(project) = params.get("projectKey").filter(|v| !v.is_undefined())\n                && false',
     'archive_matrix_matches_ts', '#1154 archive configured fallback and explicit key boundary'),
    ('archive-empty-matrix', REGRESSION,
     'let oracle = archive_fixture();\n    negative_assert_eq!(oracle["node"]',
     'let mut oracle = archive_fixture();\n    oracle["cases"] = serde_json::json!([]);\n    negative_assert_eq!(oracle["node"]',
     'archive_matrix_matches_ts', '#1154 archive matrix cardinality'),
    ('archive-node-version', ORACLE,
     '"node": "v22.22.0"', '"node": "v0.0.0"',
     'archive_matrix_matches_ts', '#1154 archive pinned TS Node'),
    ('archive-cwd-dimension', ORACLE,
     '"name": "manifest_missing_deleted",\n      "method": "manifest",\n      "key": "missing",\n      "cwd": "deleted",',
     '"name": "manifest_missing_deleted",\n      "method": "manifest",\n      "key": "missing",\n      "cwd": "project",',
     'archive_matrix_matches_ts', '#1154 archive cwd dimension'),
    ('archive-key-dimension', ORACLE,
     '"name": "manifest_missing_deleted",\n      "method": "manifest",\n      "key": "missing",',
     '"name": "manifest_missing_deleted",\n      "method": "manifest",\n      "key": "absolute",',
     'archive_matrix_matches_ts', '#1154 archive key dimension'),
    ('archive-method-dimension', ORACLE,
     '"name": "manifest_missing_deleted",\n      "method": "manifest",',
     '"name": "manifest_missing_deleted",\n      "method": "entries",',
     'archive_matrix_matches_ts', '#1154 archive method dimension'),
    ('archive-cwd-state-dimension', ORACLE,
     '"name": "manifest_missing_deleted",\n      "method": "manifest",\n      "key": "missing",\n      "cwd": "deleted",\n      "cwdReadable": false,',
     '"name": "manifest_missing_deleted",\n      "method": "manifest",\n      "key": "missing",\n      "cwd": "deleted",\n      "cwdReadable": true,',
     'archive_matrix_matches_ts', '#1154 archive cwd state dimension'),
    ('ledger-admission-drift', LEDGER,
     'The isolated native process serves only its configured project.',
     'The isolated native process admits only its own project.',
     'archive_matrix_matches_ts',
     '#1154 archive admission expectation stays bound to its ledger'),
]

if __name__ == '__main__':
    raise SystemExit(matrix_main(ROOT, __file__, PREFIX, MUTATIONS,
        passed_count=1, child_count=36, exact=True,
        paths=[REQUESTS, NATIVE_ARCHIVE, REGRESSION, LEDGER, ORACLE]))

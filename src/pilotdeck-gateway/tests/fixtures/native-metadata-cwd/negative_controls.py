#!/usr/bin/env python3
"""#1154 HEAD-only controls. --check; --compile-only OUT; OUT [names]."""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
# matrix_main executes the mutation inside these shared guarded primitives.
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import matrix_main

ROOT = Path(__file__).resolve().parents[5]
REQUESTS = ROOT / 'src/pilotdeck-gateway/src/native_process/requests.rs'
SERVICES = ROOT / 'src/pilotdeck-gateway/src/native_process/services.rs'
METADATA = ROOT / 'src/pilotdeck-gateway/src/metadata.rs'
PROJECT_FILES = ROOT / 'src/pilotdeck-gateway/src/project_files.rs'
REGRESSION = ROOT / 'src/pilotdeck-gateway/src/native_process/metadata_cwd_tests.rs'
ORACLE = Path(__file__).resolve().parent / 'oracle.json'
# fix-r3 (review P2-5): the oracle_bounded precheck's baseline module and the
# shared mask table are judges and stay guarded.
BASELINE = ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs'
MASKS = ROOT / 'src/pilotdeck-gateway/tests/common/regeneration_masks.rs'
MASK_TABLE = ROOT / 'src/pilotdeck-gateway/tests/fixtures/_shared/regeneration-masks.tsv'
PREFIX = 'native_process::tests::metadata_cwd::'
# Forwarded child receipts use the parent name, not the ignored worker name.
MUTATIONS = [
    ('request-project-base', REQUESTS,
     'cwd: std::path::PathBuf::new(),', 'cwd: self.options.project.clone(),',
     'matrix_matches_ts', '#1154 request metadata uses process cwd'),
    ('submit-project-base', SERVICES,
     'cwd: std::path::PathBuf::new(),', 'cwd: self.options.project.clone(),',
     'matrix_matches_ts', '#1154 submit metadata uses process cwd'),
    ('request-eager-cwd', REQUESTS,
     'cwd: std::path::PathBuf::new(),',
     'cwd: std::env::current_dir().map_err(|e| RequestError::new(e.to_string()))?,',
     'matrix_matches_ts', '#1154 request metadata uses process cwd'),
    ('submit-eager-cwd', SERVICES,
     'cwd: std::path::PathBuf::new(),',
     'cwd: std::env::current_dir().map_err(|e| failure(e.to_string()))?,',
     'matrix_matches_ts', '#1154 submit metadata uses process cwd'),
    ('marker-stat-lexical', METADATA,
     '.stat(&self.cwd.join(&marker))\n                .is_ok_and(|stat| stat.directory)',
     '.stat(&resolve(&self.cwd, &marker))\n                .is_ok_and(|stat| stat.directory)',
     'matrix_matches_ts', '#1154 request metadata uses process cwd'),
    ('registration-skips-cwd-error', METADATA,
     'let root = resolve_process_path(&self.cwd, Path::new(&root))?;',
     'let root = resolve(&self.cwd, &root);',
     'matrix_matches_ts', '#1154 request metadata uses process cwd'),
    ('empty-matrix', REGRESSION,
     'let oracle = fixture();\n    negative_assert_eq!(oracle["node"]',
     'let mut oracle = fixture();\n    oracle["cases"] = serde_json::json!([]);\n    negative_assert_eq!(oracle["node"]',
     'matrix_matches_ts', '#1154 matrix cardinality'),
    ('direct-project-not-first', METADATA,
     'if matches(&direct_id)? {', 'if false && matches(&direct_id)? {',
     'matrix_matches_ts', '#1154 request metadata uses process cwd'),
    ('description-loses-legacy', METADATA,
     'Err(_) => legacy = Some(candidate),', 'Err(_) => {},',
     'matrix_matches_ts', '#1154 description preserves legacy candidate on cwd error'),
    ('oracle-node-version', ORACLE,
     '"node": "v22.22.0"', '"node": "v0.0.0"',
     'matrix_matches_ts', '#1154 pinned TS Node'),
    ('list-ignores-cwd-error', METADATA,
     'if resolve_process_path(&self.cwd, Path::new(&root))? == self.home {',
     'if resolve(&self.cwd, &root) == self.home {',
     'matrix_matches_ts', '#1154 list RPC propagates cwd error'),
    ('file-rpc-short-circuit', PROJECT_FILES,
     'let listed = metadata\n        .list_projects()',
     'if metadata.has_registered_project(&requested).unwrap_or(false) { return Ok(requested); }\n    let listed = metadata\n        .list_projects()',
     'matrix_matches_ts', '#1154 file RPC enumerates all projects'),
    ('registry-follows-scan-symlink', METADATA,
     'if directory && id != direct_id && matches(&id)? {',
     'if (directory || self.files.stat(&projects.join(&id)).is_ok_and(|s| s.directory)) && id != direct_id && matches(&id)? {',
     'matrix_matches_ts', '#1154 request metadata uses process cwd'),
    ('cwd-refuses-same-project', SERVICES,
     '.is_ok_and(|cwd| crate::metadata::resolve(&self.0, &cwd) == self.0)',
     '.is_ok_and(|cwd| crate::metadata::resolve(&self.0, &cwd) != self.0)',
     'matrix_matches_ts', '#1154 isolated same-project cwd accepted'),
    ('cwd-allows-project-switch', SERVICES,
     '.is_ok_and(|cwd| crate::metadata::resolve(&self.0, &cwd) == self.0)',
     '.is_ok_and(|cwd| !cwd.is_empty())',
     'matrix_matches_ts', '#1154 isolated project switch remains refused'),
    ('chat-continues-after-cwd-error', METADATA,
     'let Ok((marker_path, marker_on_disk)) = paths(&marker) else {\n                break;',
     'let Ok((marker_path, marker_on_disk)) = paths(&marker) else {\n                continue;',
     'matrix_matches_ts', '#1154 chat lookup stops on cwd error'),
    ('chat-never-selects-collision', METADATA,
     'if identity.is_some()\n                && marker_path == target',
     'if identity.is_none()\n                && marker_path == target',
     'matrix_matches_ts', '#1154 chat lookup stops on cwd error'),
    ('file-rpc-returns-storage', PROJECT_FILES,
     'if key == requested {\n                    return Ok(key);',
     'if key == requested {\n                    return Ok(metadata.home.join("projects").join(crate::metadata::project_id(&requested)));',
     'matrix_matches_ts', '#1154 file RPC enumerates all projects'),
    ('raw-session-override', ORACLE,
     '"sessionCwd": "."', '"sessionCwd": "/project"',
     'matrix_matches_ts', '#1154 TS stores raw session cwd'),
]

if __name__ == '__main__':
    raise SystemExit(matrix_main(ROOT, __file__, PREFIX, MUTATIONS,
        passed_count=1, child_count=8, exact=True,
        paths=[REQUESTS, SERVICES, METADATA, PROJECT_FILES, REGRESSION, ORACLE,
               BASELINE, MASKS, MASK_TABLE]))

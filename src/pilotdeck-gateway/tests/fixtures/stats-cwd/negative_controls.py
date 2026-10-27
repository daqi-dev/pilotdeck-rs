#!/usr/bin/env python3
"""#946 HEAD-only nonce receipt controls; account for every selected mutant.

--check validates anchors. --compile-only compiles every selected mutant.
Provide a new output directory and optional mutation names; CARGO_TARGET_DIR
must be supplied. Commit first: the source guard is never bypassed.
"""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
# negative_suite owns mutating_sources, run_owned and wait_for_slot.
from negative_suite import matrix_main

ROOT = Path(__file__).resolve().parents[5]
STATS = ROOT / 'src/pilotdeck-gateway/src/runtime/model/stats.rs'
ROUTER = ROOT / 'src/pilotdeck-gateway/src/runtime/model/router.rs'
FIXTURE = Path(__file__).resolve().parent
PREFIX = 'runtime::model::stats::cwd_tests::'
COLLECTOR = '#879 stats cwd collector matrix:'
RUNTIME = '#879 stats cwd runtime matrix:'
CWD = '''    let cwd = std::env::current_dir().map_err(|error| {
        let (_, message) = crate::node_io::diagnostic_or_unknown(&error, "uv_cwd", None);
        std::io::Error::new(error.kind(), message)
    })?;'''
DELETED_RELATIVE = ['relative_deleted_cwd', 'empty_deleted_cwd',
                    'empty_home_unset_deleted_cwd', 'empty_home_child_deleted_cwd']
# `tilde_deleted_cwd` is unreachable for the `absolute-guard` mutant: a bare
# `PILOT_HOME="~"` returns at stats.rs:1215 before `expanded.is_absolute()`.
# Requiring all four cells turns a correct three-cell kill into INVALID (#879b).
# Only this mutation accepts any declared cell; every other row still needs all.
DELETED_ABSOLUTE = ['absolute_deleted_cwd', 'tilde_deleted_cwd',
                    'tilde_child_deleted_cwd', 'unset_deleted_cwd']
ANY_CELL = {'absolute-guard'}
NORMAL_RELATIVE = ['relative_normal_cwd', 'empty_normal_cwd',
                   'empty_home_unset_normal_cwd', 'empty_home_child_normal_cwd']
# Every cell that reads the home for its path, for the runtime entry's home
# selection. `disabled-*` returns before any home is read and `file-path-*`
# uses `config.filePath`, so a wrong home is invisible in both.
ENABLED = [f'{kind}_{state}_cwd'
           for kind in ('relative', 'empty', 'absolute', 'tilde', 'tilde_child',
                        'unset', 'empty_home_tilde', 'empty_home_unset',
                        'empty_home_child')
           for state in ('normal', 'deleted')]
MUTATIONS = [
    ('cwd-fallback', STATS, CWD,
     '    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));',
     DELETED_RELATIVE, COLLECTOR),
    ('cwd-message', STATS, 'diagnostic_or_unknown(&error, "uv_cwd", None)',
     'diagnostic_or_unknown(&error, "read", None)', DELETED_RELATIVE, COLLECTOR),
    ('error-kind', STATS, 'std::io::Error::new(error.kind(), message)',
     'std::io::Error::new(std::io::ErrorKind::PermissionDenied, message)',
     DELETED_RELATIVE, COLLECTOR),
    ('absolute-guard', STATS, 'if expanded.is_absolute() {', 'if false {',
     DELETED_ABSOLUTE, COLLECTOR),
    ('tilde-verbatim', STATS, 'if raw.as_deref() == Some("~") {', 'if false {',
     ['empty_home_tilde_normal_cwd', 'empty_home_tilde_deleted_cwd'], COLLECTOR),
    ('relative-base', STATS, 'Ok(crate::metadata::resolve_path(&cwd, &expanded))',
     'Ok(crate::metadata::resolve_path(&cwd, &expanded).join("mutated"))',
     NORMAL_RELATIVE, COLLECTOR),
    ('router-suffix', STATS, '.join("router"),', '.join("mutated-router"),',
     ['relative_normal_cwd', 'unset_normal_cwd'], COLLECTOR),
    ('disabled-guard', STATS, 'if !enabled {', 'if false {',
     ['disabled_normal_cwd', 'disabled_deleted_cwd'], COLLECTOR),
    ('file-path-branch', STATS,
     'Some(file) => file.parent().unwrap_or(Path::new(".")).to_path_buf(),',
     'Some(file) => file.parent().unwrap_or(Path::new(".")).join("mutated").to_path_buf(),',
     ['file_path_normal_cwd', 'file_path_deleted_cwd'], COLLECTOR),
    ('router-swallow', ROUTER,
     '''                let collector =
                    TokenStatsCollector::new(Some(&stats_config), None).map_err(|error| {
                        // The collector only fails on the cwd read; keep the
                        // Node error kind on the wire like the TS throw.
                        let code = match error.kind() {
                            std::io::ErrorKind::NotFound => "ENOENT",
                            std::io::ErrorKind::PermissionDenied => "EACCES",
                            _ => "UNKNOWN",
                        };
                        RouterRuntimeError::new(code, error.to_string())
                    })?;''',
     '''                let collector = match TokenStatsCollector::new(Some(&stats_config), None) {
                    Ok(collector) => collector,
                    Err(_) => TokenStatsCollector::with_clock(None, None, Arc::new(|| 0.0))
                        .map_err(|error| RouterRuntimeError::new("ENOENT", error.to_string()))?,
                };''',
     DELETED_RELATIVE, RUNTIME),
    ('runtime-home', ROUTER,
     'TokenStatsCollector::new(Some(&stats_config), None)',
     'TokenStatsCollector::new(Some(&stats_config), Some(std::path::Path::new("/wrong")))',
     ENABLED, RUNTIME),
    ('router-code', ROUTER, 'std::io::ErrorKind::NotFound => "ENOENT",',
     'std::io::ErrorKind::NotFound => "BROKEN",', DELETED_RELATIVE, RUNTIME),
    ('router-message', ROUTER, 'RouterRuntimeError::new(code, error.to_string())',
     'RouterRuntimeError::new(code, "mutated")', DELETED_RELATIVE, RUNTIME),
]



def main():
    return matrix_main(ROOT, __file__, PREFIX, MUTATIONS,
                       passed_count=22, child_count=44,
                       paths=[STATS, ROUTER, FIXTURE / "regression.rs", FIXTURE / "oracle.json", FIXTURE / "generate.mts"],
                       any_cell=ANY_CELL, exact=False)


if __name__ == '__main__':
    raise SystemExit(main())

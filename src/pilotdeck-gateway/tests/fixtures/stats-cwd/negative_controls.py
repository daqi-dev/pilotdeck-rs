#!/usr/bin/env python3
"""Committed-tree controls: PASS -> named runtime assertion RED -> restored PASS.

Every mutation is attempted and recorded as KILLED / SURVIVED / INVALID before
the script exits; the first failure does not abort the run. Run by name in
small batches. Uncommitted source is intentionally refused unless the caller
points NEGATIVE_BASELINE at a temporary commit object for a pre-submit sample.
"""
import difflib
import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import case_name, cell_hits

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


def write_new(path, text):
    with path.open('x') as stream:
        stream.write(text)


def check_anchors(mutations):
    # Validate before reporting anything about mutants. A stale anchor is an
    # apparatus error, never a surviving mutant or a test failure.
    problems = []
    for name, source, old, new, tests, marker in mutations:
        count = source.read_text().count(old)
        if count != 1 or old == new or not tests or not marker:
            problems.append(f'{name}: invalid mutation anchor (count={count})')
    if problems:
        raise RuntimeError('; '.join(problems))
    print(f'{len(mutations)} unique anchors checked', flush=True)


def run(out, label, env):
    slots = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=600, interval=15)
    write_new(out / f'{label}.slots.log', slots.stdout + slots.stderr)
    command = ['cargo', 'test', '--offline', '-q', '-p', 'pilotdeck-gateway', '--lib', PREFIX,
               '--', '--nocapture', '--format=pretty', '--test-threads=1']
    write_new(out / f'{label}.command.json', json.dumps({
        'command': command, 'cwd': str(ROOT / 'src'),
        'CARGO_TARGET_DIR': env['CARGO_TARGET_DIR']}, indent=2) + '\n')
    result = run_owned(command, cwd=ROOT / 'src', env=env, timeout=600)
    log = result.stdout + result.stderr
    write_new(out / f'{label}.log', log)
    return result, log


def passed(result, log):
    # One bounded child per entry: 22 cells x collector/runtime = 44 receipts.
    return (result.returncode == 0 and '22 passed; 0 failed; 2 ignored;' in log
            and log.count('PDRS_VERIFIED_CHILD ') == 44)


def failure_reason(result, log, name, tests, marker):
    if result.returncode == 0:
        return 'assertion did not fail'
    if 'error[' in log or 'could not compile' in log:
        return 'compile failure'
    hit, unrelated = cell_hits(log, PREFIX, tests, marker, case_name)
    if unrelated:
        return f'declared cells failed without the dedicated marker: {unrelated}'
    missed = [test for test in tests if test not in hit]
    if name in ANY_CELL:
        return f'no declared cell was red with the marker: {missed}'
    return f'declared cells stayed green: {missed}'


def killed(result, log, name, tests, marker):
    if result.returncode != 101 or 'panicked at' not in log:
        return False
    if 'error[' in log or 'could not compile' in log:
        return False
    hit, unrelated = cell_hits(log, PREFIX, tests, marker, case_name)
    if unrelated:
        return False
    return len(hit) == len(tests) if name not in ANY_CELL else bool(hit)


def record(results, name, status, reason):
    results.append({'name': name, 'status': status, 'reason': reason})
    print(f'{name}: {status} ({reason})', flush=True)


def main():
    if sys.argv[1:] == ['--check']:
        check_anchors(MUTATIONS)
        return
    env = os.environ.copy()
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    selected = sys.argv[2:]
    mutations = [m for m in MUTATIONS if not selected or m[0] in selected]
    if not mutations or (selected and len(mutations) != len(selected)):
        raise SystemExit('unknown or duplicate mutation')
    check_anchors(mutations)
    # Include the assertions, inputs and oracle in the same HEAD guard. The
    # default is HEAD; NEGATIVE_BASELINE only lets a pre-submit sample point at
    # a temporary `git stash create`/`commit-tree` object without moving HEAD.
    baseline = os.environ.get('NEGATIVE_BASELINE', 'HEAD')
    paths = [STATS, ROUTER, FIXTURE / 'regression.rs', FIXTURE / 'oracle.json',
             FIXTURE / 'generate.mts', Path(__file__).resolve()]
    results = []
    for name, source, old, new, tests, marker in mutations:
        try:
            with mutating_sources(ROOT, paths, baseline=baseline) as originals:
                original = originals[source].decode()
                if original.count(old) != 1:
                    raise RuntimeError('stale anchor after locking')
                positive, log = run(out, f'{name}-positive', env)
                if not passed(positive, log):
                    record(results, name, 'INVALID', 'positive run is not green')
                    continue
                changed = original.replace(old, new, 1)
                write_new(out / f'{name}.patch', ''.join(difflib.unified_diff(
                    original.splitlines(True), changed.splitlines(True),
                    fromfile=str(source.relative_to(ROOT)),
                    tofile=str(source.relative_to(ROOT)))))
                source.write_text(changed)
                negative, log = run(out, f'{name}-negative', env)
                if negative.returncode == 0:
                    record(results, name, 'SURVIVED', f'assertion did not fail on {tests}')
                    continue
                if not killed(negative, log, name, tests, marker):
                    reason = failure_reason(negative, log, name, tests, marker)
                    record(results, name, 'INVALID', f'{reason}; expected RED on {tests}')
                    continue
                hits, _ = cell_hits(log, PREFIX, tests, marker, case_name)
            with mutating_sources(ROOT, paths, baseline=baseline):
                restored, log = run(out, f'{name}-restored', env)
                if not passed(restored, log):
                    record(results, name, 'INVALID', 'restored run is not green')
                    continue
            record(results, name, 'KILLED', ', '.join(hits))
        except subprocess.TimeoutExpired:
            record(results, name, 'INVALID', 'timeout')
        except Exception as error:  # apparatus or setup error, never a kill
            record(results, name, 'INVALID', str(error))
    write_new(out / 'summary.json', json.dumps(results, indent=2) + '\n')
    if any(result['status'] != 'KILLED' for result in results):
        raise SystemExit(1)


if __name__ == '__main__':
    main()

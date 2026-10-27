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
from negative_suite import judge

ROOT = Path(__file__).resolve().parents[5]
MODEL = ROOT / 'src/pilotdeck-gateway/src/model_catalog.rs'
DISPATCH = ROOT / 'src/pilotdeck-gateway/src/dispatch.rs'
FIXTURE = Path(__file__).resolve().parent
PREFIX = 'model_catalog::cwd_tests::'
WORKER = 'environment_worker'  # regression.rs's bounded case worker thread
NORMAL = [f'{kind}_normal_cwd' for kind in
          ['relative', 'empty', 'absolute', 'tilde', 'tilde_child', 'unset']]
RELATIVE = ['relative_deleted_cwd', 'empty_deleted_cwd']
ABSOLUTE = [f'{kind}_deleted_cwd' for kind in ['absolute', 'tilde', 'tilde_child', 'unset']]
NATIVE = 'native metadata matrix:'
CATALOG = 'catalog route matrix:'
SESSION = 'session metadata route matrix:'
BOUNDARY = 'if home_path(env).is_none_or(|home| home.is_absolute()) {'
MUTATIONS = [
    ('cwd-swallowed', MODEL, BOUNDARY, 'if true {', RELATIVE, NATIVE),
    ('absolute-home-error', MODEL, BOUNDARY, 'if home_path(env).is_none() {', ABSOLUTE, NATIVE),
    ('missing-home-error', MODEL, BOUNDARY, 'if false {', ['missing_home_deleted_cwd'], NATIVE),
    ('cwd-message', MODEL, 'diagnostic_or_unknown(&error, "uv_cwd", None)',
     'diagnostic_or_unknown(&error, "read", None)', RELATIVE, NATIVE),
    ('cwd-code', MODEL, 'return Err(config::Error {\n                code: code.into(),',
     'return Err(config::Error {\n                code: "BROKEN".into(),', RELATIVE, NATIVE),
    ('cwd-name', MODEL, 'name: "Error",\n                requires_host: false,',
     'name: "TypeError",\n                requires_host: false,', RELATIVE, NATIVE),
    ('normal-home-layout', MODEL, 'Some(Metadata {\n        home,\n        cwd,',
     'Some(Metadata {\n        home: home.join("mutated"),\n        cwd,', NORMAL, NATIVE),
    ('normal-cwd-layout', MODEL, 'Some(Metadata {\n        home,\n        cwd,',
     'Some(Metadata {\n        home,\n        cwd: cwd.join("mutated"),', NORMAL, NATIVE),
    ('missing-home-default', MODEL, 'let system_home = env.get("HOME")?;',
     'let system_home = env.get("HOME").map(String::as_str).unwrap_or("/unexpected");',
     ['missing_home_normal_cwd'], NATIVE),
    ('catalog-swallow', DISPATCH,
     'Err(error) => metadata_error_response(request, &error),\n    }\n}',
     'Err(_error) => Route::Forward,\n    }\n}', RELATIVE, CATALOG),
    ('session-swallow', DISPATCH,
     'Err(error) => metadata_error_response(request, &error),\n            };',
     'Err(_error) => Route::Forward,\n            };', RELATIVE, SESSION),
    ('catalog-normal-forward', DISPATCH,
     'Ok(Some(metadata)) => crate::model_catalog::response(request, &metadata, env)\n'
     '            .map_or(Route::Forward, Route::LocalResponse),',
     'Ok(Some(_metadata)) => Route::Forward,', NORMAL, CATALOG),
    ('frame-code', DISPATCH, '("code", JsValue::String(error.code.as_str().into())),',
     '("code", "gateway_request_failed".into()),', RELATIVE, CATALOG),
    ('frame-message', DISPATCH, '("message", JsValue::String(error.message.clone())),',
     '("message", "mutated".into()),', RELATIVE, CATALOG),
    ('frame-id', DISPATCH, 'request.get("id").expect("validated request id").clone(),',
     '"mutated-id".into(),', RELATIVE, CATALOG),
]


def write_new(path, text):
    with path.open('x') as stream:
        stream.write(text)


def check_anchors(mutations):
    # Validate before reporting anything about mutants. A stale anchor is an
    # apparatus error, never a surviving mutant or a test failure.
    for name, source, old, new, tests, marker in mutations:
        count = source.read_text().count(old)
        if count != 1 or old == new or not tests or not marker:
            raise RuntimeError(f'{name}: invalid mutation anchor (count={count})')
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
    # One bounded child per cell: 14 cells = 14 receipts.
    return (result.returncode == 0 and '14 passed; 0 failed; 2 ignored;' in log
            and log.count('PDRS_VERIFIED_CHILD ') == 14)


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
    paths = [MODEL, DISPATCH, FIXTURE / 'regression.rs', FIXTURE / 'oracle.json',
             FIXTURE / 'config.json', FIXTURE / 'generate.mts', Path(__file__).resolve()]
    results = []
    for name, source, old, new, tests, marker in mutations:
        try:
            with mutating_sources(ROOT, paths, baseline=baseline) as originals:
                original = originals[source].decode()
                if original.count(old) != 1:
                    raise RuntimeError(f'{name}: stale anchor after locking')
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
                killed_ok, reason, hits = judge(negative, PREFIX, tests, marker, WORKER)
                if not killed_ok:
                    record(results, name, 'INVALID', f'{reason}; expected RED on {tests}')
                    continue
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

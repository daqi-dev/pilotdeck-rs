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
META = ROOT / 'src/pilotdeck-gateway/src/metadata.rs'
PAIR = ROOT / 'src/pilotdeck-gateway/src/pairing.rs'
FIXTURE = Path(__file__).resolve().parent
PREFIX = 'metadata::from_env_tests::'
CWD = '''let cwd = std::env::current_dir().map_err(|error| {
            let (_, message) = crate::node_io::diagnostic_or_unknown(&error, "uv_cwd", None);
            io::Error::new(error.kind(), message)
        })?;'''
NORMAL = ['relative_normal_cwd', 'tilde_normal_cwd', 'tilde_child_normal_cwd',
          'absolute_normal_cwd', 'empty_normal_cwd']
DELETED = ['relative_deleted_cwd', 'tilde_deleted_cwd', 'tilde_child_deleted_cwd',
           'absolute_deleted_cwd', 'empty_deleted_cwd']
MUTATIONS = [
    ('cwd-none', META, CWD,
     'let Ok(cwd) = std::env::current_dir() else { return Ok(None); };',
     DELETED, 'metadata from_env matrix:'),
    ('cwd-message', META, 'diagnostic_or_unknown(&error, "uv_cwd", None)',
     'diagnostic_or_unknown(&error, "read", None)', DELETED, 'metadata from_env matrix:'),
    ('normal-home-layout', META, 'Ok(Some(Self {\n            home,\n            cwd,',
     'Ok(Some(Self {\n            home: home.join("mutated"),\n            cwd,',
     NORMAL, 'metadata from_env matrix:'),
    ('normal-cwd-layout', META, 'Ok(Some(Self {\n            home,\n            cwd,',
     'Ok(Some(Self {\n            home,\n            cwd: cwd.join("mutated"),',
     NORMAL, 'metadata from_env matrix:'),
    ('unset-default-home', META,
     'let Ok(home) = std::env::var("PILOT_HOME") else {\n            return Ok(None);\n        };',
     'let Ok(home) = std::env::var("PILOT_HOME") else {\n'
     '            return Ok(Some(Self { home: PathBuf::from("/default-home"), '
     'cwd: PathBuf::from("/"), files: NativeFiles }));\n        };',
     ['unset_normal_cwd', 'unset_deleted_cwd'], 'metadata from_env matrix:'),
    ('missing-home-default', META,
     'let Ok(user_home) = std::env::var("HOME") else {\n                return Ok(None);\n            };',
     'let Ok(user_home) = std::env::var("HOME") else {\n'
     '                return Ok(Some(Self { home: PathBuf::from("/default-home"), '
     'cwd, files: NativeFiles }));\n            };',
     ['missing_home_normal_cwd'], 'metadata from_env matrix:'),
    ('pairing-swallow', PAIR, 'crate::metadata::Metadata::from_env()?',
     'crate::metadata::Metadata::from_env().unwrap_or(None)',
     DELETED, 'pairing propagates cwd failure:'),
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
    command = ['cargo', 'test', '-q', '-p', 'pilotdeck-gateway', '--lib', PREFIX,
               '--', '--nocapture', '--format=pretty', '--test-threads=1']
    write_new(out / f'{label}.command.json', json.dumps({
        'command': command, 'cwd': str(ROOT / 'src'),
        'CARGO_TARGET_DIR': env['CARGO_TARGET_DIR']}, indent=2) + '\n')
    result = run_owned(command, cwd=ROOT / 'src', env=env, timeout=600)
    log = result.stdout + result.stderr
    write_new(out / f'{label}.log', log)
    return result, log


def passed(result, log):
    # One bounded child per cell: 13 cells = 13 receipts.
    return (result.returncode == 0 and '13 passed; 0 failed; 2 ignored;' in log
            and log.count('PDRS_VERIFIED_CHILD ') == 13)


def failure_reason(result, log, tests, marker):
    if result.returncode == 0:
        return 'assertion did not fail'
    if 'error[' in log or 'could not compile' in log:
        return 'compile failure'
    hit, unrelated = cell_hits(log, PREFIX, tests, marker, case_name)
    if unrelated:
        return f'declared cells failed without the dedicated marker: {unrelated}'
    missed = [test for test in tests if test not in hit]
    return f'declared cells stayed green: {missed}'


def killed(result, log, tests, marker):
    if result.returncode != 101 or 'panicked at' not in log:
        return False
    if 'error[' in log or 'could not compile' in log:
        return False
    hit, unrelated = cell_hits(log, PREFIX, tests, marker, case_name)
    return not unrelated and len(hit) == len(tests)


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
    paths = [META, PAIR, FIXTURE / 'regression.rs', FIXTURE / 'oracle.json',
             FIXTURE / 'generate.mts', Path(__file__).resolve()]
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
                if not killed(negative, log, tests, marker):
                    reason = failure_reason(negative, log, tests, marker)
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

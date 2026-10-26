#!/usr/bin/env python3
"""Committed-tree controls: PASS -> runtime assertion RED -> restored PASS.

Run by name in small batches. Uncommitted source is intentionally refused.
"""
import difflib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

ROOT = Path(__file__).resolve().parents[5]
META = ROOT / 'src/pilotdeck-gateway/src/metadata.rs'
PAIR = ROOT / 'src/pilotdeck-gateway/src/pairing.rs'
TEST = ROOT / 'src/pilotdeck-gateway/tests/fixtures/metadata-from-env/regression.rs'
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
    assert result.returncode == 0 and '13 passed; 0 failed; 2 ignored;' in log, log
    assert log.count('PDRS_VERIFIED_CHILD ') == 13, 'all matrix receipts required'


def main():
    env = os.environ.copy()
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    selected = sys.argv[2:]
    mutations = [m for m in MUTATIONS if not selected or m[0] in selected]
    assert mutations and (not selected or len(mutations) == len(selected)), 'unknown/duplicate mutation'
    results = []
    for name, source, old, new, tests, marker in mutations:
        # Guard the test and both product files as well as the whole gateway
        # product tree. Only the default HEAD baseline is permitted here.
        with mutating_sources(ROOT, [META, PAIR, TEST]) as originals:
            original = originals[source].decode()
            assert original.count(old) == 1, (name, original.count(old))
            positive, log = run(out, f'{name}-positive', env)
            passed(positive, log)
            changed = original.replace(old, new, 1)
            write_new(out / f'{name}.patch', ''.join(difflib.unified_diff(
                original.splitlines(True), changed.splitlines(True),
                fromfile=str(source.relative_to(ROOT)), tofile=str(source.relative_to(ROOT)))))
            source.write_text(changed)
            negative, log = run(out, f'{name}-negative', env)
            killed = (negative.returncode == 101 and 'panicked at' in log
                      and all(f'{PREFIX}{test} ... FAILED' in log
                              and f'{marker} {test.removesuffix("_cwd").replace("_", "-")}' in log
                              for test in tests))
            assert killed, f'{name}: compile/setup/timeout failures do not count as a kill'
        # The shared context restored and verified HEAD bytes. Reacquire the
        # same guard before the restored positive run, without writing bytes.
        with mutating_sources(ROOT, [META, PAIR, TEST]):
            restored, log = run(out, f'{name}-restored', env)
            passed(restored, log)
        results.append(dict(name=name, killed=True, restoredPass=True, tests=tests))
        write_new(out / f'{name}.result.json', json.dumps(results[-1], indent=2) + '\n')
        print(f'{name}: runtime RED, restored PASS', flush=True)
    write_new(out / 'summary.json', json.dumps(results, indent=2) + '\n')


if __name__ == '__main__':
    main()

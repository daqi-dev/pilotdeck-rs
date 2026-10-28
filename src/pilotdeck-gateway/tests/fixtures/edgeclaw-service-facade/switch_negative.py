#!/usr/bin/env python3
"""#988 HEAD-only controls for the feature boundary and exact selector.

Commit the new mutants.rs first. No alternate baseline, source copy or target.
Usage: CARGO_TARGET_DIR=/abs/target python3 switch_negative.py NEW_OUTPUT_DIR
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import time
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import classify, passed

ROOT = Path(__file__).resolve().parents[5]
SOURCE = ROOT / 'src/pilotdeck-gateway/src/mutants.rs'
MUTATIONS = [
    ('default-enabled', SOURCE, '    false\n}', '    true\n}',
     'mutants::tests::switches_are_disabled_without_feature'),
    ('selector-inverted', SOURCE, 'selected == name', 'selected != name',
     'mutants::tests::selection_matches_only_the_requested_name'),
]
MESSAGES = {
    'default-enabled': '#988 non-feature switch must be constant false',
    'selector-inverted': '#988 selected switch',
}


def main():
    if len(sys.argv) != 2 or not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('NEW_OUTPUT_DIR and caller CARGO_TARGET_DIR are required')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, PDRS_MUTANT='default-root-dir')
    results = []

    def publish():
        counts = {status: sum(r['status'] == status for r in results)
                  for status in ('KILLED', 'SURVIVED', 'INVALID')}
        temp = out / 'summary.json.tmp'
        temp.write_text(json.dumps({'controls': results, 'counts': counts}, indent=2) + '\n')
        temp.replace(out / 'summary.json')

    def run(name, test, phase, compile_only=False):
        command = ['cargo', 'test', '--offline', '--locked', '--manifest-path', str(ROOT / 'src/Cargo.toml'),
                   '-p', 'pilotdeck-gateway', '--lib']
        if name == 'selector-inverted':
            command += ['--features', 'pdrs-mutants']
        command += ['--no-run'] if compile_only else [test, '--', '--exact', '--nocapture']
        with (out / f'{name}-{phase}.log').open('w') as log:
            try:
                slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=900)
                (out / f'{name}-{phase}.slots.log').write_text(slot.stdout + slot.stderr)
                result = run_owned(command, cwd=ROOT, env=env, timeout=900)
                log.write(result.stdout + result.stderr)
            except Exception as error:
                if isinstance(error, subprocess.TimeoutExpired):
                    for part in (error.stdout, error.stderr):
                        log.write(part.decode(errors='replace') if isinstance(part, bytes) else part or '')
                log.write(f'\n{type(error).__name__}: {error}\n')
                raise
        return result

    try:
        with mutating_sources(ROOT, [SOURCE]) as originals:
            for name, path, old, new, test in MUTATIONS:
                status, reason = 'INVALID', ''
                try:
                    source = originals[path].decode()
                    if source.count(old) != 1:
                        raise ValueError(f'{name}: stale anchor')
                    if not passed(run(name, test, 'positive'), test):
                        raise ValueError('positive test failed')
                    path.write_text(source.replace(old, new, 1))
                    time.sleep(1.1)
                    if run(name, test, 'compile', True).returncode:
                        raise ValueError('mutant does not compile')
                    status, reason = classify(run(name, test, 'negative'),
                        SimpleNamespace(test=test, assertion=MESSAGES[name]))
                except Exception as error:
                    status, reason = 'INVALID', str(error)
                finally:
                    path.write_bytes(originals[path])
                    time.sleep(1.1)
                try:
                    if not passed(run(name, test, 'restored'), test):
                        status, reason = 'INVALID', reason + '; restored test failed'
                except Exception as error:
                    status, reason = 'INVALID', reason + '; restored: ' + str(error)
                results.append(dict(name=name, status=status, reason=reason))
                publish()
    except Exception as error:
        for row in results:
            row.update(status='INVALID', reason=str(error))
        for name, *_ in MUTATIONS[len(results):]:
            results.append(dict(name=name, status='INVALID', reason='source guard: ' + str(error)))
    publish()
    for row in results:
        print(f"{row['name']}: {row['status']}: {row['reason']}")
    return int(any(r['status'] != 'KILLED' for r in results))


if __name__ == '__main__':
    sys.exit(main())

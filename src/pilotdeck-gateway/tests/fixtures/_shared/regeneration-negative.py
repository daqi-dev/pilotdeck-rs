#!/usr/bin/env python3
"""#280 exact regeneration controls; compile each mutant, run all, then summarize.

Usage: regeneration-negative.py OUT [--compile-only] [mutation ...]
No fixture/source copy or alternate cargo target is created. Integration-target
execution uses negative_suite's exact-test classifier and the shared source guard.
"""
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, passed

root = Path(__file__).resolve().parents[5]
provider = root / 'src/pilotdeck-gateway/tests/fixtures/provider-timing/oracle.json'
registry = root / 'src/pilotdeck-gateway/tests/fixtures/run-registry/oracle.json'
snapshot = root / 'src/pilotdeck-gateway/tests/fixtures/run-registry/normal.after.json'
cases = [
    ('provider-unmasked-byte', provider,
     '"sourceCommit": "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"',
     '"sourceCommit": "dc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"',
     'provider_timing_regenerates_identically',
     '#280 provider-timing/oracle.json differs from committed bytes outside the exact masks'),
    ('registry-unmasked-byte', registry,
     '"sourceCommit": "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"',
     '"sourceCommit": "dc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"',
     'run_registry_regenerates_identically',
     '#280 run-registry/oracle.json differs from committed bytes outside the exact masks'),
    ('snapshot-unmasked-byte', snapshot, '"schemaVersion":1', '"schemaVersion":2',
     'run_registry_regenerates_identically',
     '#280 run-registry/normal.after.json differs from committed bytes outside the exact masks'),
]


def main():
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('#280 CARGO_TARGET_DIR must be supplied by the caller')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    compile_only = '--compile-only' in sys.argv[2:]
    names = [arg for arg in sys.argv[2:] if arg != '--compile-only']
    selected = [case for case in cases if not names or case[0] in names]
    if names and len(selected) != len(set(names)):
        raise SystemExit('#280 unknown mutation name')
    env = os.environ.copy()
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--test', 'generator_provenance']
    results = []

    def run(label, args):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
        (out / (label + '.slots.log')).write_text(slot.stdout + slot.stderr)
        result = run_owned(command + args, cwd=root, env=env, timeout=900)
        (out / (label + '.log')).write_text(result.stdout + result.stderr)
        return result

    def record(name, test, assertion, status, reason):
        results.append(dict(mutation=name, test=test, assertion=assertion,
                            status=status, reason=reason))
        print(f'{name}: {status}: {reason}', flush=True)

    try:
        with mutating_sources(root, list(dict.fromkeys(case[1] for case in selected))) as originals:
            for name, path, old, new, test, assertion in selected:
                status, reason = 'INVALID', 'not executed'
                args = [test, '--', '--exact', '--ignored', '--nocapture']
                try:
                    content = originals[path].decode()
                    if content.count(old) != 1:
                        raise RuntimeError(f'#280 stale anchor {name}: count={content.count(old)}')
                    changed = content.replace(old, new, 1)
                    json.loads(changed)
                    if not compile_only and not passed(run(name + '-positive', args), test):
                        raise RuntimeError('#280 positive bound test did not pass')
                    path.write_text(changed)
                    if run(name + '-compile', ['--no-run']).returncode:
                        raise RuntimeError('#280 mutant did not compile')
                    if compile_only:
                        status, reason = 'COMPILED', 'mutant compiles; runtime classification not requested'
                    else:
                        status, reason = classify(run(name + '-negative', args),
                                                  Case(name, path, old, new, test, assertion))
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    path.write_bytes(originals[path])
                try:
                    restored = run(name + '-restored', ['--no-run'] if compile_only else args)
                    if restored.returncode or (not compile_only and not passed(restored, test)):
                        raise RuntimeError('#280 restored control did not pass')
                except Exception as error:
                    status, reason = 'INVALID', reason + '; ' + str(error)
                record(name, test, assertion, status, reason)
    except Exception as error:
        for row in results:
            row.update(status='INVALID', reason='source guard: ' + str(error))
        for name, _path, _old, _new, test, assertion in selected[len(results):]:
            record(name, test, assertion, 'INVALID', 'source guard: ' + str(error))
    states = ('COMPILED', 'INVALID') if compile_only else ('KILLED', 'SURVIVED', 'INVALID')
    counts = {state: sum(row['status'] == state for row in results) for state in states}
    (out / 'summary.json').write_text(json.dumps(dict(controls=results, counts=counts), indent=2) + '\n')
    print(json.dumps(counts), flush=True)
    expected = 'COMPILED' if compile_only else 'KILLED'
    return int(not results or any(row['status'] != expected for row in results))


if __name__ == '__main__':
    raise SystemExit(main())

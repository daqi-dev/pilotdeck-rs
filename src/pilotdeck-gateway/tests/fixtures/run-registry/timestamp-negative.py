#!/usr/bin/env python3
"""#1129: remove the generator clock and require the same-millisecond probe to fail.

Run after committing, with caller-owned CARGO_TARGET_DIR and PDRS_BASELINE.
Run exclusively in this worktree: no concurrent builds, tests or source writers.
The source guard intentionally refuses uncommitted sources and new files.
"""
import os
from pathlib import Path
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import invalidate_completed, publish_summary

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/tests/fixtures/run-registry/generate.mts'
probe = root / 'src/pilotdeck-gateway/tests/fixtures/run-registry/verify-timestamps.mts'
mutations = [
    ('remove-snapshot-clock', source,
     'globalThis.Date = SnapshotDate;', 'globalThis.Date = NativeDate;'),
]
MARKER = '#1129 same-millisecond recovery masks only the new timestamp'
PASS = 'PASS: #1129 same-millisecond timestamps are masked and seed timestamps preserved'


def main():
    if sys.argv[1:] == ['--check-anchors']:
        for name, path, old, new in mutations:
            if path.read_text().count(old) != 1 or old == new:
                raise SystemExit(f'{name}: expired or invalid anchor')
        print(f'{len(mutations)} unique anchors checked')
        return 0
    if len(sys.argv) != 2:
        raise SystemExit('usage: timestamp-negative.py NEW_OUTPUT_DIR | --check-anchors')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR') or not env.get('PDRS_BASELINE'):
        raise SystemExit('caller must supply CARGO_TARGET_DIR and PDRS_BASELINE')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    results = []

    def run(label, *, compile=False):
        if compile:
            slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600, interval=15)
            (out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
            command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                       '-p', 'pilotdeck-gateway', '--test', 'generator_provenance', '--no-run']
        else:
            command = ['node', '--import',
                       str(Path(env['PDRS_BASELINE']) / 'node_modules/tsx/dist/loader.mjs'),
                       str(probe), str(out / label)]
        result = run_owned(command, cwd=root, env=env, timeout=900 if compile else 150)
        (out / f'{label}.stdout').write_text(result.stdout)
        (out / f'{label}.stderr').write_text(result.stderr)
        return result

    def passed(result):
        return result.returncode == 0 and PASS in result.stdout.splitlines()

    try:
        with mutating_sources(root, [source, probe, Path(__file__).resolve()]) as originals:
            for name, path, old, new in mutations:
                row = dict(mutation=name, test=probe.name, assertion=MARKER,
                           status='INVALID', reason='not run', compiled=False)
                try:
                    text = originals[path].decode()
                    if text.count(old) != 1 or old == new:
                        raise ValueError('expired or invalid anchor')
                    if not passed(run(name + '-positive')):
                        raise RuntimeError('positive probe failed')
                    path.write_text(text.replace(old, new, 1))
                    if run(name + '-compile', compile=True).returncode != 0:
                        raise RuntimeError('mutant did not compile')
                    row['compiled'] = True
                    result = run(name + '-negative')
                    if passed(result):
                        row.update(status='SURVIVED', reason='bound probe still passed')
                    elif (result.returncode == 1 and
                          f'AssertionError [ERR_ASSERTION]: {MARKER}' in result.stderr.splitlines()):
                        row.update(status='KILLED', reason='real TS same-millisecond mask assertion failed')
                    else:
                        row.update(reason='missing bound runtime assertion; never a kill')
                except Exception as error:
                    row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                finally:
                    path.write_bytes(originals[path])
                try:
                    if not passed(run(name + '-restored')):
                        raise RuntimeError('restored positive probe failed')
                except Exception as error:
                    row.update(status='INVALID', reason=row['reason'] + f'; restored: {error}')
                results.append(row)
    except BaseException as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for name, *_ in mutations[len(results):]:
            results.append(dict(mutation=name, test=probe.name, assertion=MARKER,
                                status='INVALID', reason=reason, compiled=False))
    # Shared publisher atomically exposes results only after source restoration.
    publish_summary(out, results)
    for row in results:
        print(f'{row["mutation"]}: {row["status"]}: {row["reason"]}', flush=True)
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())

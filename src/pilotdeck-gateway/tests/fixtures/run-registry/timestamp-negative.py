#!/usr/bin/env python3
"""#1129: bind clock and exclusive-write mutations to real TS probe assertions.

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
     'globalThis.Date = SnapshotDate;', 'globalThis.Date = NativeDate;',
     'run-registry/verify-timestamps.mts::verifyTimestamps',
     '#1129 same-millisecond recovery masks only the new timestamp'),
    ('remove-clock-restoration', source,
     '  globalThis.Date = NativeDate;', '  // mutant: leave the snapshot clock installed',
     'run-registry/verify-timestamps.mts::verifyTimestamps',
     '#1129 generator restores its caller clock'),
    ('ignore-explicit-date-arguments', source,
     'args.length ? args : [1760745600000 + tick++]', '[1760745600000 + tick++]',
     'run-registry/verify-timestamps.mts::verifyTimestamps',
     '#1129 generator explicit Date arguments stay native'),
    ('overwrite-initial-fixture', source,
     'await copyFile(path, join(output, initialFile), constants.COPYFILE_EXCL);',
     'await copyFile(path, join(output, initialFile), 0);',
     'run-registry/verify-timestamps.mts::verifyExclusiveInitial',
     '#1129 initial copy refuses to overwrite existing fixture'),
    ('overwrite-after-fixture', source,
     'await copyFile(path, join(output, after), constants.COPYFILE_EXCL);',
     'await copyFile(path, join(output, after), 0);',
     'run-registry/verify-timestamps.mts::verifyExclusiveAfter',
     '#1129 after copy refuses to overwrite existing fixture'),
    ('overwrite-oracle-fixture', source,
     "await writeFile(join(output, 'oracle.json'), oracle, { flag: 'wx' });",
     "await writeFile(join(output, 'oracle.json'), oracle, { flag: 'w' });",
     'run-registry/verify-timestamps.mts::verifyExclusiveOracle',
     '#1129 oracle publication refuses to overwrite existing fixture'),
]


def main():
    if sys.argv[1:] == ['--check-anchors']:
        for name, path, old, new, _test, _marker in mutations:
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

    def run(label, test, *, compile=False):
        if compile:
            slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600, interval=15)
            (out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
            # Required Rust build receipt; .mts mutations are executed by Node below.
            command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                       '-p', 'pilotdeck-gateway', '--test', 'generator_provenance', '--no-run']
        else:
            command = ['node', '--import',
                       str(Path(env['PDRS_BASELINE']) / 'node_modules/tsx/dist/loader.mjs'),
                       str(probe), str(out / label), test.rsplit('::', 1)[1]]
        result = run_owned(command, cwd=root, env=env, timeout=900 if compile else 150)
        (out / f'{label}.stdout').write_text(result.stdout)
        (out / f'{label}.stderr').write_text(result.stderr)
        return result

    def passed(result, test):
        lines = result.stdout.splitlines()
        return (result.returncode == 0 and lines.count(f'TEST: {test}') == 1
                and lines.count(f'PASS: {test}') == 1)

    try:
        with mutating_sources(root, [source, probe, Path(__file__).resolve()]) as originals:
            for name, path, old, new, test, marker in mutations:
                row = dict(mutation=name, test=test, assertion=marker,
                           status='INVALID', reason='not run', compiled=False)
                try:
                    text = originals[path].decode()
                    if text.count(old) != 1 or old == new:
                        raise ValueError('expired or invalid anchor')
                    if not passed(run(name + '-positive', test), test):
                        raise RuntimeError('positive probe failed')
                    path.write_text(text.replace(old, new, 1))
                    if run(name + '-compile', test, compile=True).returncode != 0:
                        raise RuntimeError('mutant did not compile')
                    row['compiled'] = True
                    result = run(name + '-negative', test)
                    if passed(result, test):
                        row.update(status='SURVIVED', reason='bound probe still passed')
                    elif (result.returncode == 1 and
                          result.stdout.splitlines().count(f'TEST: {test}') == 1 and
                          f'PASS: {test}' not in result.stdout.splitlines() and
                          f'AssertionError [ERR_ASSERTION]: {marker}' in result.stderr.splitlines()):
                        row.update(status='KILLED', reason='bound real TS probe assertion failed')
                    else:
                        row.update(reason='missing bound runtime assertion; never a kill')
                except Exception as error:
                    row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                finally:
                    path.write_bytes(originals[path])
                try:
                    if not passed(run(name + '-restored', test), test):
                        raise RuntimeError('restored positive probe failed')
                except Exception as error:
                    row.update(status='INVALID', reason=row['reason'] + f'; restored: {error}')
                results.append(row)
    except BaseException as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for name, _path, _old, _new, test, marker in mutations[len(results):]:
            results.append(dict(mutation=name, test=test, assertion=marker,
                                status='INVALID', reason=reason, compiled=False))
    # Shared publisher atomically exposes results only after source restoration.
    publish_summary(out, results)
    for row in results:
        print(f'{row["mutation"]}: {row["status"]}: {row["reason"]}', flush=True)
    print('SUMMARY: ' + ' '.join(
        f'{status}={sum(row["status"] == status for row in results)}'
        for status in ('KILLED', 'SURVIVED', 'INVALID')), flush=True)
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())

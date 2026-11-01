#!/usr/bin/env python3
"""#360 fix-r3 (r3 review P3-1). Run after committing; OUT [NAME ...].

The transport zone-file read bound: a TZif above the shared 1 MiB cap must be
rejected as a whole, never silently truncated and handed to the parser. Every
mutant must compile, fail the bound assertion, and pass after restoration.
All selected outcomes are reported; --check-anchors is read-only.
"""
import argparse
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, passed, publish_summary

root = Path(__file__).resolve().parents[5]
system_clock = root / 'src/pilotdeck-transport/src/system_clock.rs'
bound = 'zone_read_above_the_1mib_cap_is_not_admitted'
cases = [
    # Dropping the post-read rejection restores the pre-fix truncation: the
    # one-past-the-cap byte is still read, but the oversized file is returned
    # to the parser anyway. Both bound assertions are kill markers because a
    # different cap mutation can break either side of the boundary.
    ('zone-read-cap-ignored', system_clock,
     '(bytes.len() <= 1_048_576).then_some(bytes)', 'Some(bytes)', bound,
     ['#360 a 1 MiB zone file must be admitted in full',
      '#360 a zone file above the 1 MiB cap must not be admitted (nor truncated)']),
]


def main():
    if sys.argv[1:] == ['--check-anchors']:
        failures = []
        for name, path, old, _, _, _ in cases:
            count = path.read_text().count(old)
            if count != 1:
                failures.append(f'{name}: expected 1 anchor, got {count}')
        print('\n'.join(failures) if failures else f'{len(cases)} anchors OK')
        return int(bool(failures))
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out', type=Path)
    parser.add_argument('mutations', nargs='*')
    args = parser.parse_args()
    env = os.environ.copy()
    if not env.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be provided by the caller')
    unknown = set(args.mutations) - {row[0] for row in cases}
    if unknown:
        parser.error(f'unknown mutations: {sorted(unknown)}')
    selected = [row for row in cases if not args.mutations or row[0] in args.mutations]
    args.out.mkdir(parents=True, exist_ok=False)
    results = []

    def run(label, test, compile_only=False):
        command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                   '-p', 'pilotdeck-transport', '--test', 'system_clock']
        try:
            slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=300)
            (args.out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
            # No `--nocapture`: the bound test prints nothing, and a clean
            # status line keeps the FAILED judge exact (#938 r14).
            suffix = ['--no-run'] if compile_only else [test, '--', '--exact']
            result = run_owned(command + suffix, cwd=root, env=env, timeout=600)
            (args.out / f'{label}.log').write_text(result.stdout + result.stderr)
            return result
        except Exception as error:
            raw = ''
            if isinstance(error, subprocess.TimeoutExpired):
                for item in (error.stdout, error.stderr):
                    raw += item.decode(errors='replace') if isinstance(item, bytes) else item or ''
            with (args.out / f'{label}.log').open('a') as log:
                log.write(raw + f'\n{type(error).__name__}: {error}\n')
            raise

    def record(name, test, assertion, status, reason):
        results.append(dict(mutation=name, test=test, assertion=assertion, status=status, reason=reason))
        print(f'{name}: {status}: {reason}', flush=True)

    try:
        with mutating_sources(root, sorted({row[1] for row in selected})) as snapshots:
            for name, path, old, new, test, assertion in selected:
                status, reason = 'INVALID', 'not run'
                try:
                    original = snapshots[path].decode()
                    if original.count(old) != 1:
                        raise ValueError(f'{name}: expected one source anchor')
                    if not passed(run(name + '-positive', test), test):
                        reason = 'bound positive test did not pass'
                    else:
                        path.write_text(original.replace(old, new, 1))
                        if run(name + '-compile', test, compile_only=True).returncode != 0:
                            reason = 'mutant failed to compile'
                        else:
                            status, reason = classify(run(name + '-negative', test), Case(name, path, old, new, test, assertion))
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    path.write_bytes(snapshots[path])
                try:
                    if not passed(run(name + '-restored', test), test):
                        status, reason = 'INVALID', reason + '; restored bound test did not pass'
                except Exception as error:
                    status, reason = 'INVALID', reason + f'; restored: {error}'
                record(name, test, assertion, status, reason)
    except Exception as error:
        reason = f'source guard: {error}'
        for row in results:
            row.update(status='INVALID', reason=row['reason'] + '; ' + reason)
        for name, _, _, _, test, assertion in selected[len(results):]:
            record(name, test, assertion, 'INVALID', reason)
    counts = {state: sum(row['status'] == state for row in results) for state in ('KILLED', 'SURVIVED', 'INVALID')}
    publish_summary(args.out, results)
    print(counts, flush=True)
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())

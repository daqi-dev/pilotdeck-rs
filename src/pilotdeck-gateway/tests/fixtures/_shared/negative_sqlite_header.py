#!/usr/bin/env python3
# anchor-selfcheck: unsupported two-file dynamic byte mutation executor; anchors are checked below
"""#979: prove the SQLite canonicaliser is load-bearing and still content-strict.

Each mutant corrupts one page byte *after* the writer-version field has been
canonicalised. The bound regeneration guard must fail at its byte comparison;
the untouched generator must pass before and after the mutation.

This script mutates tracked fixture generators in place, so it must run on the
committed tree (``mutating_sources`` refuses a working-tree baseline mismatch).
Requires ``PDRS_BASELINE`` and a caller-provided ``CARGO_TARGET_DIR``.
"""
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, new_output_directory, run_owned, wait_for_slot
from negative_suite import Case, classify as classify_case, invalidate_completed, passed

ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GENERATOR = FIXTURES / 'edgeclaw-accessors/generate.mts'
STORAGE_OBSERVER = FIXTURES / 'edgeclaw-storage/observe.mts'

# (name, path, before, after, test, assertion)
MUTANT_ROWS = [
    (
        'accessors-page-byte',
        GENERATOR,
        "base64:canonicalSqliteBase64(join(root,'workspaces',c.id,'control.sqlite'))",
        "base64:(()=>{const b=Buffer.from(canonicalSqliteBase64(join(root,'workspaces',c.id,'control.sqlite')),'base64');b[100]^=1;return b.toString('base64');})()",
        'edgeclaw_accessors_regenerates_identically',
        '#280 edgeclaw-accessors/oracle.json differs from the regenerated',
    ),
    (
        'storage-page-byte',
        STORAGE_OBSERVER,
        '  return canonicalSqliteBase64(dbPath);',
        '  const bytes=Buffer.from(canonicalSqliteBase64(dbPath),\'base64\');bytes[100]^=1;return bytes.toString(\'base64\');',
        'edgeclaw_storage_regenerates_identically',
        '#280 edgeclaw-storage/oracle.json differs from the regenerated',
    ),
]
# No `--nocapture`: the guard's `println!` would otherwise be spliced between
# `test <name> ... ` and `ok`/`FAILED`, so the shared single-line judge in
# `negative_suite.py` scores a passing positive control as INVALID.
COMMAND = [
    'cargo', 'test', '--offline', '--locked',
    '--manifest-path', 'src/Cargo.toml',
    '-p', 'pilotdeck-gateway', '--test', 'generator_provenance',
    '--', '--ignored', '--exact', '--test-threads=1',
]


def run(output, env, label, test):
    slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=1800)
    (output / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
    result = run_owned(COMMAND + [test], cwd=ROOT, env=env, timeout=1800)
    (output / f'{label}.log').write_text(result.stdout + result.stderr)
    return result


def classify(result, test, assertion):
    # Reuse the repository judge so a kill still requires the full named test
    # and a runtime panic, never a build error or an unrelated failure.
    return classify_case(result, Case('', None, '', '', test, assertion))


def write_summary(output, results):
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    (output / 'summary.json').write_text(
        json.dumps({'controls': results, 'counts': counts}, indent=2) + '\n')
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()))


def main():
    output = new_output_directory(Path(sys.argv[1]))
    baseline = os.environ.get('PDRS_BASELINE')
    if not baseline:
        raise SystemExit('#979 set PDRS_BASELINE to the fixed PilotDeck checkout')
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('#979 set CARGO_TARGET_DIR to the caller-owned cargo target')
    env = dict(os.environ)
    env['PDRS_BASELINE'] = baseline
    results = []
    try:
        with mutating_sources(ROOT, [GENERATOR, STORAGE_OBSERVER]) as originals:
            original = {path: originals[path].decode() for path in originals}
            for name, path, before, _after, _test, _assertion in MUTANT_ROWS:
                assert original[path].count(before) == 1, (name, 'anchor must be unique')
            for name, path, before, after, test, assertion in MUTANT_ROWS:
                status, reason = 'INVALID', ''
                try:
                    if not passed(run(output, env, f'{name}-positive', test), test):
                        reason = 'positive control did not pass'
                    else:
                        changed = original[path].replace(before, after, 1)
                        path.write_bytes(changed.encode())
                        status, reason = classify(
                            run(output, env, f'{name}-negative', test), test, assertion)
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    path.write_bytes(originals[path])
                    if path.read_bytes() != originals[path]:
                        raise RuntimeError(f'#979 byte restoration failed: {path}')
                try:
                    if not passed(run(output, env, f'{name}-restored', test), test):
                        status, reason = 'INVALID', reason + '; restored bound test did not pass'
                except Exception as error:
                    status, reason = (
                        'INVALID', reason + f'; restored: {type(error).__name__}: {error}')
                results.append({'mutation': name, 'test': test,
                                'status': status, 'reason': reason})
                print(f'{name}: {status}: {reason}', flush=True)
                write_summary(output, results)
    except Exception as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        # The guard exits after restoring and re-verifying every mutated source.
        # A failure there invalidates rows already recorded as KILLED; never
        # leave them green and exit 0 (#979 P0). Shared with negative_suite.
        invalidate_completed(results, reason)
        for name, _path, _before, _after, test, _assertion in MUTANT_ROWS[len(results):]:
            results.append({'mutation': name, 'test': test,
                            'status': 'INVALID', 'reason': reason})
    write_summary(output, results)
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())

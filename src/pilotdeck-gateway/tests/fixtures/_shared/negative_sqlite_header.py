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
    """Publish the one final summary atomically, after restore verification.

    Rows are accumulated in memory while the guarded mutation window is open
    (#1076). A temp file plus rename keeps a reader from observing a
    half-written JSON document; the payload shape is unchanged (#166 r8).
    """
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    target = Path(output) / 'summary.json'
    temporary = target.with_name(target.name + '.tmp')
    temporary.write_text(json.dumps({'controls': results, 'counts': counts}, indent=2) + '\n')
    temporary.replace(target)
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
    # Same three-state exit contract as ``negative_suite.run_cases``
    # (#1076 fix-r1): a refusal before the guarded window opened keeps the
    # historical rc=1, only an unverified restore or a cancellation is rc=2.
    phase = 'enter'
    try:
        with mutating_sources(ROOT, [GENERATOR, STORAGE_OBSERVER]) as originals:
            phase = 'body'
            original = {path: originals[path].decode() for path in originals}
            for name, path, before, _after, _test, _assertion in MUTANT_ROWS:
                count = original[path].count(before)
                if count != 1:
                    # Stale anchors fail closed: no row may be recorded as
                    # SURVIVED just because the mutation never applied (#1076).
                    raise ValueError(
                        f'#1076 {name} anchor expired: count={count}, expected=1')
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
                    phase = 'restore'
                    path.write_bytes(originals[path])
                    if path.read_bytes() != originals[path]:
                        raise RuntimeError(f'#979 byte restoration failed: {path}')
                    phase = 'body'
                try:
                    if not passed(run(output, env, f'{name}-restored', test), test):
                        status, reason = 'INVALID', reason + '; restored bound test did not pass'
                except Exception as error:
                    status, reason = (
                        'INVALID', reason + f'; restored: {type(error).__name__}: {error}')
                results.append({'mutation': name, 'test': test,
                                'status': status, 'reason': reason})
                print(f'{name}: {status}: {reason}', flush=True)
            # Done with the body: a failure from here on comes from the guard's
            # own restore/re-verify on context exit.
            phase = 'exit'
    except BaseException as error:
        # The guard exits after restoring and re-verifying every mutated source.
        # A failure there — including a SIGINT or a raw SystemExit — invalidates
        # rows already recorded as KILLED; never leave them green, and never
        # publish a KILLED summary from a cancelled run (#979 P0, #1076). The
        # final summary was memory-only until now, so an interrupt during this
        # publish leaves no summary (missing is fail-closed), not the old file.
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for name, _path, _before, _after, test, _assertion in MUTANT_ROWS[len(results):]:
            results.append({'mutation': name, 'test': test,
                            'status': 'INVALID', 'reason': reason})
        write_summary(output, results)
        if isinstance(error, SystemExit):
            raise SystemExit(2) from error
        # #1076 fix-r1: rc=2 only for an unverified restore or a cancellation;
        # a guard refusal to run (baseline mismatch, busy lock) stays rc=1.
        if phase in ('restore', 'exit') or not isinstance(error, Exception):
            return 2
        return 1
    write_summary(output, results)
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())

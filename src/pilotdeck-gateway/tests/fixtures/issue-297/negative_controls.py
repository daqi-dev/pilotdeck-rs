#!/usr/bin/env python3
"""#297 stale-snapshot restore: in-place mutants over the merge guard.

Both controls mutate the same guard in TranscriptWriter::restore_recorder_entries
(src/pilotdeck-gateway/src/storage/transcript_adapter.rs). The guard keeps the
further-committed writer position when a caller restores an older snapshot:

- restore-rewinds-stale-writer disables the guard (``if true``), restoring the
  pre-fix unconditional overwrite. The #297 reproduction test must FAIL.
- restore-never-installs makes the guard never install (``if false``), so even a
  fresh recorder's first restore cannot seed the writer. The existing positive
  control must FAIL, proving the guarded code path is load-bearing.

Run every selected control, then summarize; any non-KILLED control exits
non-zero. Anchors that no longer match the committed source are INVALID (an
expired anchor never counts as SURVIVED). Requires the caller's
CARGO_TARGET_DIR; cargo always runs through wait_for_slot + run_owned, never
quiet.
"""
import json
import os
import sys
from pathlib import Path

root = Path(__file__).resolve().parents[5]
fixtures = root / 'src/pilotdeck-gateway/tests/fixtures'
sys.path[:0] = [str(fixtures), str(fixtures / 'turn-runner')]
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, passed
from _negative_common import mutate

source = root / 'src/pilotdeck-gateway/src/storage/transcript_adapter.rs'
prefix = 'runtime::project::factory::tests::persistence_tests::'
guard = 'if state.sequence > inner.sequence {'
cases = [
    ('restore-rewinds-stale-writer', source, guard, 'if true {', None,
     prefix + 'stale_snapshot_restore_must_not_corrupt_the_transcript_chain',
     '#297 a stale snapshot restore wrote a transcript the reader can no longer load'),
    ('restore-never-installs', source, guard, 'if false {', None,
     prefix + 'persistence_real_recorder_survives_runtime_restart_and_isolates_sessions',
     '#297 the reopened session writer must retain the persisted parent chain'),
]


def build_env():
    env = os.environ.copy()
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('caller must set CARGO_TARGET_DIR')
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only',
               CARGO_PROFILE_TEST_DEBUG='line-tables-only',
               CARGO_TERM_QUIET='false')
    return env


def main():
    args = sys.argv[1:]
    if not args:
        raise SystemExit('usage: negative_controls.py OUT [mutation names]')
    out = Path(args[0])
    out.mkdir(parents=True, exist_ok=False)
    names = set(args[1:])
    if names - {case[0] for case in cases}:
        raise SystemExit('unknown mutation')
    chosen = [case for case in cases if not names or case[0] in names]
    chosen = [(name, path, old, new, test, assertion)
               for name, path, old, new, _scope, test, assertion in chosen]
    env = build_env()
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--lib']

    def run(label, test, compile_only=False):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
        (out / (label + '.slots.log')).write_text(slot.stdout + slot.stderr)
        argv = ['--no-run'] if compile_only else [test, '--', '--exact', '--include-ignored']
        result = run_owned(command + argv, cwd=root, env=env, timeout=900)
        with (out / (label + '.log')).open('x') as log:
            log.write(result.stdout + result.stderr)
        return result

    results = []
    try:
        with mutating_sources(root, [source]) as originals:
            for name, old, new, test, assertion in chosen:
                row = dict(mutation=name, test=test, assertion=assertion,
                           status='INVALID', reason='', compiled=False)
                try:
                    if not passed(run(name + '-positive', test), test):
                        raise RuntimeError('bound positive test failed')
                    changed = mutate(originals[source].decode(), None, [(old, new)])
                    if changed == originals[source].decode():
                        raise ValueError('mutation leaves source unchanged')
                    source.write_text(changed)
                    if run(name + '-compile', test, compile_only=True).returncode:
                        raise RuntimeError('mutant compile failed')
                    row['compiled'] = True
                    status, reason = classify(
                        run(name + '-negative', test),
                        Case(name, source, old, new, test, assertion))
                    row.update(status=status, reason=reason)
                except Exception as error:
                    row.update(reason=f'{type(error).__name__}: {error}')
                finally:
                    source.write_bytes(originals[source])
                if not passed(run(name + '-restored', test), test):
                    row.update(status='INVALID',
                               reason=row['reason'] + '; restored test failed')
                results.append(row)
                print(f'{name}: {row["status"]}: {row["reason"]}', flush=True)
    except Exception as error:
        # A failed pristine/restore guard invalidates every unreported row;
        # it must surface as an error exit, never as SURVIVED.
        recorded = {row['mutation'] for row in results}
        for name, _path, _old, _new, test, assertion in chosen:
            if name not in recorded:
                results.append(dict(mutation=name, test=test, assertion=assertion,
                                    status='INVALID', compiled=False,
                                    reason=f'guard: {error}'))

    counts = {status: sum(row['status'] == status for row in results)
              for status in ('KILLED', 'SURVIVED', 'INVALID')}
    with (out / 'summary.json').open('x') as summary:
        json.dump(dict(controls=results, counts=counts), summary, indent=2)
        summary.write('\n')
    print(f'summary: KILLED={counts["KILLED"]}, SURVIVED={counts["SURVIVED"]}, '
          f'INVALID={counts["INVALID"]}', flush=True)
    raise SystemExit(int(any(row['status'] != 'KILLED' for row in results)))


if __name__ == '__main__':
    main()

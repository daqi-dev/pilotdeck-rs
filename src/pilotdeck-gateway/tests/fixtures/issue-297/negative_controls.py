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
non-zero (1). A guard failure while restoring the sources revokes every
recorded row through `negative_suite.invalidate_completed` and exits 2
("restore unverified") instead — a possibly dirty tree must never publish an
all-green summary (#979). Anchors that no longer match the committed source are
INVALID (an expired anchor never counts as SURVIVED). Requires the caller's
CARGO_TARGET_DIR; cargo always runs through wait_for_slot + run_owned, never
quiet.
"""
import os
import sys
from pathlib import Path

root = Path(__file__).resolve().parents[5]
fixtures = root / 'src/pilotdeck-gateway/tests/fixtures'
sys.path[:0] = [str(fixtures), str(fixtures / 'turn-runner')]
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import (
    Case,
    classify,
    invalidate_completed,
    passed,
    publish_summary,
)
from _negative_common import mutate

source = root / 'src/pilotdeck-gateway/src/storage/transcript_adapter.rs'
prefix = 'runtime::project::factory::tests::persistence_tests::'
# fix-r1 re-anchor: the guard moved from `if state.sequence > inner.sequence {`
# to a named `installed` flag (`Recorder::restore` must not re-seed its own
# projection when the writer refused the snapshot, so the helper has to hand
# the decision back). The mutants are unchanged in meaning — `if true {` is
# still the pre-fix unconditional install and `if false {` still never
# installs — only the anchor text follows the code.
guard = 'if installed {'
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
    guard_failure = None
    try:
        with mutating_sources(root, [source]) as originals:
            for name, _path, old, new, test, assertion in chosen:
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
    except BaseException as error:
        # #297 review P0 (GLM boundary #1 and concurrency #1, DeepSeek
        # sibling #1: three roles, two families). `mutating_sources` restores
        # and re-verifies the source on context exit; when it raises there,
        # the tree may not be restored and every row already recorded as
        # KILLED is untrustworthy. Voiding only the unreported rows would
        # publish an all-green summary and exit 0 from a possibly mutated tree
        # — the #979 false pass `negative_suite.invalidate_completed` is
        # written for. Exit 2 marks "restore unverified", distinct from 1
        # ("some control did not kill").
        guard_failure = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, guard_failure)
        recorded = {row['mutation'] for row in results}
        for name, _path, _old, _new, test, assertion in chosen:
            if name not in recorded:
                results.append(dict(mutation=name, test=test, assertion=assertion,
                                    status='INVALID', compiled=False,
                                    reason=guard_failure))

    counts = {status: sum(1 for row in results if row['status'] == status)
              for status in ('KILLED', 'SURVIVED', 'INVALID')}
    publish_summary(out, results)
    print(f'summary: KILLED={counts["KILLED"]}, SURVIVED={counts["SURVIVED"]}, '
          f'INVALID={counts["INVALID"]}', flush=True)
    if guard_failure is not None:
        raise SystemExit(2)
    raise SystemExit(int(not results or any(row['status'] != 'KILLED' for row in results)))


if __name__ == '__main__':
    main()

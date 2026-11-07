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

Each case runs through the shared executor (`negative_suite.run_cases`,
migrated in r5 to match issue-297-domain-fold): positive -> mutate
(unique-anchor text replacement) -> negative judged by
`negative_suite.classify` -> byte-verified restore -> restored positive. A
compile prepass proves each mutant compiles before the run; `--check` is a
read-only anchor preflight. Refusals before the run (unknown name, roster
drift, missing/unusable CARGO_TARGET_DIR, existing output directory) exit 2,
distinct from 1 ("some control did not kill"); a source-guard failure while
restoring revokes every recorded row and exits 2 (#979, #1076, #1734).
SIGINT/SIGTERM raise `SystemExit` so the guard's restore window always
unwinds and re-verifies the tree.
"""
import argparse
import json
import os
from pathlib import Path
import signal
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_selection import (claim_dir, missing_target, refuse,
                                 refuse_unusable_target, registered)
from negative_suite import Case, invalidate_completed, run_cases

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/storage/transcript_adapter.rs'
prefix = 'runtime::project::factory::tests::persistence_tests::'
# fix-r1 re-anchor: the guard moved from `if state.sequence > inner.sequence {`
# to a named `installed` flag (`Recorder::restore` must not re-seed its own
# projection when the writer refused the snapshot, so the helper has to hand
# the decision back). The mutants are unchanged in meaning — `if true {` is
# still the pre-fix unconditional install and `if false {` still never
# installs — only the anchor text follows the code.
guard = 'if installed {'
# fix-r3: fix-r1 gave the reproduction test an earlier issue-tagged assertion —
# with the pre-fix unconditional install the writer rewinds, the recorder loses
# `t2`, and `record_model_request("s2", "t2")` panics with `event_outside_turn`
# long before the reopened-chain assertion is reached. Both markers are owned by
# this test and both describe the same corruption, so the case binds the pair:
# which one the mutant trips first is an implementation detail of where the test
# notices, not a change in what the control proves.
cases = [
    ('restore-rewinds-stale-writer', source, guard, 'if true {',
     prefix + 'stale_snapshot_restore_must_not_corrupt_the_transcript_chain',
     ['#297 a stale snapshot restore wrote a transcript the reader can no longer load',
      '#297 a stale restore must not drop the turns the live writer still holds']),
    ('restore-never-installs', source, guard, 'if false {',
     prefix + 'persistence_real_recorder_survives_runtime_restart_and_isolates_sessions',
     '#297 the reopened session writer must retain the persisted parent chain'),
]
# #1734 denominator rule (r5, MM r3 sibling P1 remedy): a runtime `len(cases)`
# denominator would let a deleted row shrink the run to a smaller all-green
# summary; the roster is pinned to the two plan rows in the source.
EXPECTED = ['restore-rewinds-stale-writer', 'restore-never-installs']


def mutate(content, scope, replacements):
    for old, new in replacements:
        count = content.count(old)
        if count != 1:
            raise ValueError(f'stale anchor count={count}, expected=1: {old!r}')
        content = content.replace(old, new, 1)
    return content


def audit(rows):
    for _, path, old, new, _, _ in rows:
        mutate(path.read_text(), None, [(old, new)])


def compile_controls(rows, out, env):
    results = []
    try:
        with mutating_sources(root, list(dict.fromkeys(row[1] for row in rows))) as originals:
            for name, path, old, new, _, _ in rows:
                result = {'mutation': name, 'compiled': False, 'status': 'INVALID', 'reason': ''}
                try:
                    path.write_text(mutate(originals[path].decode(), None, [(old, new)]))
                    slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600)
                    (out / f'{name}-compile.slots.log').write_text(slot.stdout + slot.stderr)
                    run = run_owned(['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                                     '-p', 'pilotdeck-gateway', '--lib', '--no-run'],
                                    cwd=root, env=env, timeout=600)
                    (out / f'{name}-compile.log').write_text(run.stdout + run.stderr)
                    result.update(compiled=run.returncode == 0, returncode=run.returncode,
                                  status='COMPILED' if run.returncode == 0 else 'INVALID')
                except Exception as error:
                    result['reason'] = f'{type(error).__name__}: {error}'
                finally:
                    path.write_bytes(originals[path])
                results.append(result)
    except Exception as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for name, *_ in rows[len(results):]:
            results.append({'mutation': name, 'compiled': False, 'status': 'INVALID',
                            'reason': reason})
    (out / 'compile-summary.json').write_text(json.dumps(results, indent=2) + '\n')
    for result in results:
        print(json.dumps(result), flush=True)
    return all(result['compiled'] and result['status'] == 'COMPILED' for result in results)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--compile-only', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('selected', nargs='*')
    args = parser.parse_args()
    registered(cases, EXPECTED)  # roster drift is a refusal, never a shorter run
    rows = cases
    if args.selected:
        unknown = set(args.selected) - {row[0] for row in rows}
        if unknown:
            parser.error(f'unknown mutations: {sorted(unknown)}')
        rows = [row for row in rows if row[0] in args.selected]
    audit(rows)  # Stale anchors are errors, never survivors.
    if args.check:
        print(f'{len(rows)} unique anchors checked')
        return 0
    if not args.output:
        parser.error('provide a fresh evidence directory')
    env = dict(os.environ)
    if missing_target(env):
        refuse('CARGO_TARGET_DIR must be supplied by the caller')
    refuse_unusable_target(env)  # #1666: dot / root / repo / src targets too
    # Positive/compile/restored runs never inherit a stale protocol from a caller.
    env.pop('PDRS_NEGATIVE_RECEIPTS', None)
    env.pop('PDRS_NEGATIVE_PARENT_TEST', None)
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only',
               CARGO_PROFILE_TEST_DEBUG='line-tables-only', CARGO_TERM_QUIET='false')
    out = claim_dir(args.output)  # #1734: an existing directory is "never started" (2)
    # #1076: a signal must unwind through the guard's restore/re-verify window,
    # not kill the process with a mutated tree left behind.
    def stop(signum, frame):
        raise SystemExit(f'interrupted by signal {signum}')
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    compiled = compile_controls(rows, out, env)
    if args.compile_only:
        return int(not compiled)
    return run_cases(root, [Case(*row) for row in rows], out, env, mutate,
                    timeout=900, script=Path(__file__))


if __name__ == '__main__':
    raise SystemExit(main())

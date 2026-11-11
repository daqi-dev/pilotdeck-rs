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
compile prepass records compile evidence for every mutant and folds into the
exit code (a mutant that could not be proved to compile is never a green
run); `--check` is a read-only anchor preflight. Refusals before the run
(unknown or repeated name, roster drift, missing/unusable
CARGO_TARGET_DIR, existing output directory) exit 2, distinct from 1 ("some
control did not kill"); a source-guard failure while restoring revokes every
recorded row and exits 2 (#979, #1076, #1734). Once the stop handlers
installed in `main` are in place, SIGINT/SIGTERM/SIGHUP/SIGQUIT raise
`SystemExit(2)`: a cancel outside the shared guard's write-back window is
a refusal, the same code `run_cases` itself normalizes to. The startup
segment before that installation follows the default disposition (SIGTERM
143, SIGINT an uncaught KeyboardInterrupt 130). A cancel landing inside
the restore window still unwinds
and re-verifies the tree first; the guard raises `SystemExit(128+signum)`
on exit (the `negative_process.mutating_sources` deferred-signal
contract). In the judging phase `run_cases` normalizes that back to 2
with an all-INVALID summary (both windows of that phase exit 2;
`test_negative_suite.test_guard_systemexit_256_voids_last_kill_and_exits_2`
locks the normalization in). The compile preflight's own guard window is
not normalized: `SystemExit(128+signum)` passes `compile_controls`'
`except Exception` and becomes the process exit code, with no
compile-summary written.
"""
import argparse
import json
import os
from pathlib import Path
import signal
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot, tree_lock
from negative_selection import (claim_dir, missing_target, refuse,
                                 refuse_unusable_target, registered, select)
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
        if old == new:
            # A no-op replacement is a broken control; recording it as
            # SURVIVED would hide that (settings/negative_controls.py:168).
            raise ValueError('mutation leaves source unchanged')
        count = content.count(old)
        if count != 1:
            raise ValueError(f'stale anchor count={count}, expected=1: {old!r}')
        content = content.replace(old, new, 1)
    return content


def audit(rows):
    # #2795 fix-r3（sibling P1 同族跟改，manager-gateway fix-r2 先例）：锚点核对
    # 也要持树锁读——兄弟变异运行器在自己的 write 与 restore 之间可能把变异体
    # 留在盘上，无锁读会把核对错在它不拥有的源上；锁忙 / 读失败是拒跑（2）
    # （本入口在 audit 内具名拒绝，stale ValueError 仍走调用点的 refuse）。
    # audit 与后续变异轮是顺序关系，锁先释放再由 mutating_sources 重取。
    try:
        with tree_lock(root, timeout=60):
            for _, path, old, new, _, _ in rows:
                mutate(path.read_text(), None, [(old, new)])
    except (TimeoutError, OSError, UnicodeDecodeError) as error:
        print(f'--check tree lock or read failed: '
              f'{type(error).__name__}: {error}', file=sys.stderr)
        return 2
    return 0


def compile_controls(rows, out, env):
    """Compile every mutant before the run; return (all_compiled, guard_failed).

    Only the source guard's enter/exit can reach the outer ``except`` (each
    row's own failures are caught inside), so a guard that could not restore
    or re-verify the tree is reported separately: #979/#1076/#1734 make that
    a "never started" exit 2 with every recorded row revoked, not a judged
    run that merely came out not-green.
    """
    results = []
    guard_failed = False
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
        guard_failed = True
        # r7 (MM r6 concurrency P2#3): a lock-timeout means not a single byte
        # was ever written -- report "lock busy, never started", not a
        # restore failure, so triage does not go hunting for a dirty tree.
        kind = ('source lock busy (never started)'
                if isinstance(error, TimeoutError)
                else f'source guard: {type(error).__name__}: {error}')
        invalidate_completed(results, kind)
        for name, *_ in rows[len(results):]:
            results.append({'mutation': name, 'compiled': False, 'status': 'INVALID',
                            'reason': kind})
    (out / 'compile-summary.json').write_text(json.dumps(results, indent=2) + '\n')
    for result in results:
        print(json.dumps(result), flush=True)
    return (all(result['compiled'] and result['status'] == 'COMPILED' for result in results),
            guard_failed)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--compile-only', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('selected', nargs='*')
    args = parser.parse_args()
    # r6 (DS r5 + MM r5, both families): every "never started" refusal must
    # reach exit 2 through refuse()/argparse, never an uncaught traceback
    # (rc 1). The shared helpers raise ValueError; select() also refuses a
    # repeated name before it can fold a mistyped selection into a smaller
    # green run (#1666 / #1734).
    try:
        registered(cases, EXPECTED)  # roster drift is a refusal, never a shorter run
        rows = select(cases, args.selected, always=())
    except ValueError as error:
        refuse(str(error))
    # `output` is nargs='?': a bare mutation name would land there and read
    # as an evidence directory while the intended selection shrinks silently.
    if args.output and args.output in {row[0] for row in cases}:
        parser.error(f'{args.output!r} is a mutation name; '
                     'the evidence directory comes first')
    try:
        audit_rc = audit(rows)  # Stale anchors are refusals, never survivors.
    except (ValueError, OSError) as error:
        # r7 (DS r6 boundary/concurrency + MM r6 boundary, both families): a
        # missing/renamed source (FileNotFoundError, PermissionError) is an
        # OSError, not a ValueError -- it must refuse (2), never traceback (1).
        refuse(f'stale anchor: {error}')
    if audit_rc:  # #2795 fix-r3: the lock/read refusal (2) reaches the exit code.
        return audit_rc
    if args.check:
        print(f'{len(rows)} rows / '
              f'{len({(row[1], row[2]) for row in rows})} unique anchors checked')
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
    env.pop('PDRS_MUTANT', None)  # #1616: a nested caller's mutant must not leak in
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only',
               CARGO_PROFILE_TEST_DEBUG='line-tables-only', CARGO_TERM_QUIET='false')
    out = claim_dir(args.output)  # #1734: an existing directory is "never started" (2)
    # #1076: a signal must unwind through the guard's restore/re-verify window,
    # not kill the process with a mutated tree left behind. The four signals
    # are the ones the shared guard itself shields in its write-back window;
    # a cancel outside that window exits 2, matching run_cases' own
    # normalization. Inside the shared write-back window the guard defers
    # the signal, finishes the restore/verify, then raises
    # SystemExit(128+signum) (negative_process contract); run_cases
    # normalizes that back to 2 in the judging phase, while the compile
    # preflight's `except Exception` lets it through as the exit code.
    def stop(signum, frame):
        sys.stderr.write(f'interrupted by signal {signum}\n')
        raise SystemExit(2)
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    signal.signal(signal.SIGHUP, stop)
    signal.signal(signal.SIGQUIT, stop)
    compiled, guard_failed = compile_controls(rows, out, env)
    if guard_failed:
        raise SystemExit(2)
    if args.compile_only:
        return int(not compiled)
    status = run_cases(root, [Case(*row) for row in rows], out, env, mutate,
                       timeout=900, script=Path(__file__))
    # r7 (DS r6 concurrency/boundary + MM r6 concurrency/boundary, both
    # families): run_cases returns 2 when the guard could not restore or
    # re-verify the tree (every row revoked, tree possibly dirty) and 2 must
    # not be folded into 1 -- the triage table treats them differently
    # (#1076/#1734). A prepass that could not prove every mutant compiles is
    # still not a green run even when all judged rows KILLED.
    return status or int(not compiled)


if __name__ == '__main__':
    raise SystemExit(main())

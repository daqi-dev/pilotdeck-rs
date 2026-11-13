#!/usr/bin/env python3
"""#297 r1 in-place advance + shared fold: the four plan-pinned mutants.

The r1 commit replaced the per-event `DomainState` deep copy in the commit
path with the in-place `DomainState::advance(entry, &mut undo)` and an undo
journal, and converged both restore paths onto one `fold_domain`. These
controls hold the four properties the plan registered for
`tests/fixtures/issue-297-domain-fold/negative_controls.py`:

- advance-reclones-state re-introduces the per-event full-state clone
  (`std::hint::black_box(self.clone())` at the top of `advance`), the shape
  the #297 trend test exists to price: cumulative per-event cost must stay
  flat as turns accumulate. (`&mut T` has no `Clone` impl, so `self.clone()`
  on `&mut self` resolves to the owned `DomainState::clone` — verified by a
  minimal type test and by the negctl evidence, r3.)
- turn-index-never-populated stops `turn_started` from populating
  `turn_index`, so id-based turn lookup loses the turn; the #743 golden
  domain test must FAIL at its outcome assertion.
- commit-keeps-advanced-domain-on-append-failure deletes the post-append
  rollback, leaving the writer on the advanced state after a store failure;
  the #297 P2#3 differential row (FaultStore append failures) must FAIL.
- restore-fold-redivided skips the shared fold on the `SessionStorage`
  restore path (`DomainState::default()` instead of `fold_domain(entries)`),
  breaking the two-path agreement the #297 P2#3 differential test pins.

Plan-scope exemption (r4, DS r3 sibling P2; corrected r5 after kill
testing): the shared fold and the undo boundary each have a second site
this table deliberately does not mutate — the recorder half of the restore
(`Self::fold_domain(&entries)`, transcript_adapter.rs:698) and the
validate-failure rollback (:1173). The r4 text claimed the differential
positive `restore_paths_agree_on_the_shared_domain_fold` covers both
halves; kill testing split that claim in two. Replacing the :698 fold with
`DomainState::default()` fails the differential (KILLED — the recorder half
is pinned), while deleting the :1173 rollback leaves it green (SURVIVED —
a failed validate leaves no state divergence the current test surface
observes, so no registerable row can pin it: a row that survives would
break the all-KILLED contract this suite reports). Both raw outputs are in
`_tmp/20261108-issue-297-r5-staging/kill-{698,1173}-evidence/` (PR #2230
body); the validate-rollback coverage gap needs a test that observes
post-failure state, which the plan's four rows do not include. The plan
pins exactly these four rows for this fixture; the :1173 validate-rollback
gap is tracked in #1657, gated on a test that observes post-failure state
(no registerable row exists on the current test surface). Test names
and #743 assertion anchors mirror `session-domain/schemata.py:21-23`; keep
the two in sync when that fixture re-anchors.

Each case runs through the shared executor (`negative_suite.run_cases`):
positive -> mutate (unique-anchor text replacement) -> negative judged by
`negative_suite.classify` -> byte-verified restore -> restored positive. A
compile prepass records compile evidence for every mutant and folds into the
exit code (the implementer residency constraint: a mutant that could not be
proved to compile is never a green run); `--check` is a read-only anchor
preflight. Refusals before the run (unknown or repeated name, roster drift,
missing/unusable CARGO_TARGET_DIR, existing output directory) exit 2,
distinct from 1 ("some control did not kill"); a source-guard failure while
restoring revokes every recorded row and exits 2 (#979, #1076, #1734).
Once the stop handlers installed in `main` are in place,
SIGINT/SIGTERM/SIGHUP/SIGQUIT raise `SystemExit(2)`: a cancel outside the
shared guard's write-back window is a refusal, the same code `run_cases`
itself normalizes to. The startup segment before that installation
follows the default disposition (SIGTERM 143, SIGINT an uncaught
KeyboardInterrupt 130). A cancel landing inside the restore window still
unwinds and re-verifies the tree first; the guard raises
`SystemExit(128+signum)` on exit (the `negative_process.mutating_sources`
deferred-signal contract). In the judging phase `run_cases` normalizes
that back to 2 with an all-INVALID summary (both windows of that phase
exit 2;
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
domain = root / 'src/pilotdeck-gateway/src/storage/session_domain.rs'
adapter = root / 'src/pilotdeck-gateway/src/storage/transcript_adapter.rs'
session_test = 'storage::session_domain::tests::'
fold_test = ('storage::transcript_adapter_tests::'
             'restore_paths_agree_on_the_shared_domain_fold')
# The golden test asserts both outcome kinds; either assertion proves the kill.
GOLDEN_ANCHORS = ['#743 domain transition outcome', '#743 domain log outcome']
cases = [
    # The plan's cost mutant: the whole-state clone per advance is the
    # pre-#297 shape; black_box keeps the clone from being optimized away.
    ('advance-reclones-state', domain,
     '    pub(super) fn advance(&mut self, entry: &JsValue, undo: &mut Vec<Undo>) -> RecorderResult<()> {\n'
     '        let kind = (if crate::mutants::on("null-entry-property") {',
     '    pub(super) fn advance(&mut self, entry: &JsValue, undo: &mut Vec<Undo>) -> RecorderResult<()> {\n'
     '        let _ = std::hint::black_box(self.clone());\n'
     '        let kind = (if crate::mutants::on("null-entry-property") {',
     session_test + 'advance_cost_stays_flat_as_turns_accumulate',
     '#297 per-event domain cost grew'),
    ('turn-index-never-populated', domain,
     '                self.turn_index.entry(key).or_insert(slot);',
     '                let _ = (key, slot);',
     session_test + 'real_ts_domain_logs_and_events_match_bytes',
     GOLDEN_ANCHORS),
    # The append-failure tail only: the validate-failure rollback above it
    # (`return Err(error);`) keeps its rollback, so the mutant isolates the
    # post-store half of the undo boundary.
    ('commit-keeps-advanced-domain-on-append-failure', adapter,
     '        if let Err(error) = self.base.store.append(&self.base.key(&self.path), &entry) {\n'
     '            inner.recorder_domain.rollback(&mut undo);\n'
     '            return Err(E::from(AdapterError::Store(error)));\n'
     '        }',
     '        if let Err(error) = self.base.store.append(&self.base.key(&self.path), &entry) {\n'
     '            return Err(E::from(AdapterError::Store(error)));\n'
     '        }',
     fold_test,
     '#297 P2#3'),
    # restore_entries' `fold_domain(entries)` — the `(&entries)` twin at the
    # recorder restore is a different anchor, so this lands on the
    # SessionStorage half of the shared fold only.
    ('restore-fold-redivided', adapter,
     '        let domain = Self::fold_domain(entries)?;',
     '        let domain = DomainState::default();',
     fold_test,
     '#297 P2#3'),
]
# #1734 denominator rule (r4, MM r3 boundary P2): the plan pins exactly these
# four rows; a runtime `len(cases)` denominator would let a deleted row shrink
# the run to a smaller all-green summary.
EXPECTED = ['advance-reclones-state', 'turn-index-never-populated',
            'commit-keeps-advanced-domain-on-append-failure',
            'restore-fold-redivided']


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
        reason = ('source lock busy (never started)'
                  if isinstance(error, TimeoutError)
                  else f'source guard: {type(error).__name__}: {error}')
        invalidate_completed(results, reason)
        for name, *_ in rows[len(results):]:
            results.append({'mutation': name, 'compiled': False, 'status': 'INVALID',
                            'reason': reason})
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
               CARGO_PROFILE_TEST_DEBUG='line-tables-only',
               CARGO_TERM_QUIET='false')
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

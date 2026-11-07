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
pins exactly these four rows for this fixture; the unmutated twins are the
adjacent `session-domain/adapter-negative.py` rows' scope. Test names
and #743 assertion anchors mirror `session-domain/schemata.py:21-23`; keep
the two in sync when that fixture re-anchors.

Each case runs through the shared executor (`negative_suite.run_cases`):
positive -> mutate (unique-anchor text replacement) -> negative judged by
`negative_suite.classify` -> byte-verified restore -> restored positive. A
compile prepass proves each mutant compiles before the run (the implementer
residency constraint); `--check` is a read-only anchor preflight. Refusals
before the run (unknown name, roster drift, missing/unusable
CARGO_TARGET_DIR, existing output directory) exit 2, distinct from 1
("some control did not kill"); a source-guard failure while restoring
revokes every recorded row and exits 2 (#979, #1076, #1734). SIGINT/SIGTERM
raise `SystemExit` so the guard's restore window always unwinds and
re-verifies the tree.
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
               CARGO_PROFILE_TEST_DEBUG='line-tables-only',
               CARGO_TERM_QUIET='false')
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

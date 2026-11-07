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
  flat as turns accumulate.
- turn-index-never-populated stops `turn_started` from populating
  `turn_index`, so id-based turn lookup loses the turn; the #743 golden
  domain test must FAIL at its outcome assertion.
- commit-keeps-advanced-domain-on-append-failure deletes the post-append
  rollback, leaving the writer on the advanced state after a store failure;
  the #297 P2#3 differential row (FaultStore append failures) must FAIL.
- restore-fold-redivided skips the shared fold on the `SessionStorage`
  restore path (`DomainState::default()` instead of `fold_domain(entries)`),
  breaking the two-path agreement the #297 P2#3 differential test pins.

Each case: positive -> mutate (text replacement over the original bytes) ->
`cargo test --no-run` proof the mutant compiles -> negative run judged by
`negative_suite.classify` (named FAILED + this test's own panic payload
carrying the bound assertion anchor) -> restored positive. Any non-KILLED
case exits 1; a source-guard failure while restoring revokes every recorded
row (`negative_suite.invalidate_completed`) and exits 2 — a possibly dirty
tree must never publish an all-green summary (#979). An expired anchor is
INVALID, never SURVIVED. Requires the caller's CARGO_TARGET_DIR; cargo
always runs through wait_for_slot + run_owned, never quiet.
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
        with mutating_sources(root, [domain, adapter]) as originals:
            for name, path, old, new, test, assertion in chosen:
                row = dict(mutation=name, test=test, assertion=assertion,
                           status='INVALID', reason='', compiled=False)
                try:
                    if not passed(run(name + '-positive', test), test):
                        raise RuntimeError('bound positive test failed')
                    changed = mutate(originals[path].decode(), None, [(old, new)])
                    if changed == originals[path].decode():
                        raise ValueError('mutation leaves source unchanged')
                    path.write_text(changed)
                    if run(name + '-compile', test, compile_only=True).returncode:
                        raise RuntimeError('mutant compile failed')
                    row['compiled'] = True
                    status, reason = classify(
                        run(name + '-negative', test),
                        Case(name, path, old, new, test, assertion))
                    row.update(status=status, reason=reason)
                except Exception as error:
                    row.update(reason=f'{type(error).__name__}: {error}')
                finally:
                    path.write_bytes(originals[path])
                if not passed(run(name + '-restored', test), test):
                    row.update(status='INVALID',
                               reason=row['reason'] + '; restored test failed')
                results.append(row)
                print(f'{name}: {row["status"]}: {row["reason"]}', flush=True)
    except BaseException as error:
        # Same contract as the issue-297 fixture: a guard that raises on
        # context exit may leave the tree unrestored, so every recorded row
        # is untrustworthy (#979). Exit 2 marks "restore unverified",
        # distinct from 1 ("some control did not kill").
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

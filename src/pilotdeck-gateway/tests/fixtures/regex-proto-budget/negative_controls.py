#!/usr/bin/env python3
"""#1265 reverse negatives: the match budget must actually bound execution.

Five mutations of src/pilotdeck-regex-proto/src/engine.rs, each bound to one
exact test of the crate's feature-gated suites (which run only under
--features regex-proto):

  interrupt-decrement-removed     the interrupt handler never charges a poll
                                  and never interrupts -> budget.rs exhaustion
                                  test fails
  exhaustion-downgraded-to-no-match  a spent budget returns Ok(NoMatch) ->
                                  budget.rs exhaustion test fails
  budget-unarmed-between-requests the interrupted flag is not cleared between
                                  requests -> budget.rs fresh-budget test fails
  zero-budget-entry-removed        exec no longer refuses a zero allowance at
                                  its entry; only the poll-time handler is
                                  left, so a sub-granularity subject runs to
                                  a verdict -> budget.rs zero-budget test
                                  fails at its trivial-subject assertion
  cross-engine-check-removed       exec no longer rejects a pattern compiled
                                  by another engine -> lifecycle.rs
                                  cross-engine test fails

Commit first: mutating_sources proves engine.rs equals HEAD before anything is
written. Every mutant must pass `cargo test --no-run` before its negative run;
the bound test must pass before and after each mutation (restored bytes are
re-verified by the guard). All mutants run before the summary; any verdict
other than KILLED exits non-zero (verdict_exit: at least one row and all
KILLED). Mutants are published and restored with write_source_atomic (#1903),
never a truncating in-place write. Startup refusals -- unknown or duplicate
mutation names (the shared select, not a local copy), a roster that no longer
matches the static EXPECTED_NAMES list, an unusable CARGO_TARGET_DIR
(absent/empty/blank, or the dot/root/src values the shared rule rejects), an
already existing output directory (the script creates OUT itself; an existing
one means "never started") -- all exit 2 (#1734), before the evidence
directory is claimed. One refusal happens after the claim: the engine's
compile_fail doctests run once before the first mutation (CI cannot run
feature-gated tests), inside the source guard's window, and refuse (2) on a
non-zero run *or* on fewer than the 2 expected passing doctests (rc 0 with
deleted or ignored doctests is NOT_RUN as PASS), with the log kept in OUT.
The caller must set CARGO_TARGET_DIR (and the cargo slot gate's
bin dir on PATH); an expired anchor fails the row as INVALID, which also
exits non-zero.

Usage:
  negative_controls.py OUT [mutation names]
"""
import json
import os
from pathlib import Path
import re
import sys
from types import SimpleNamespace

root = Path(__file__).resolve().parents[5]
fixtures = root / 'src/pilotdeck-gateway/tests/fixtures'
sys.path[:0] = [str(fixtures), str(fixtures / 'turn-runner')]
from negative_process import (mutating_sources, run_owned, wait_for_slot,
                              write_source_atomic)
from negative_selection import (claim_dir, missing_target, refuse,
                                refuse_unusable_target, registered,
                                select, verdict_exit)
from negative_suite import classify, passed, publish_summary
import pending_reanchor as pending
from _negative_common import mutate

engine = root / 'src/pilotdeck-regex-proto/src/engine.rs'
# Tuple fields: (name, path, old, new, scope, bound test, test binary,
# assertion anchor(s)).
cases = [
    ('interrupt-decrement-removed', engine,
     '''                let previous = rem.fetch_sub(1, Ordering::SeqCst);
                if previous <= 0 {
                    hit.store(true, Ordering::SeqCst);
                    return true;
                }
                false''',
     '''                let _ = (&rem, &hit);
                false''',
     None, 'budget_states_are_independently_distinguishable', 'budget',
     '#1265 budget exhaustion must be an explicit interrupt, not a verdict'),
    ('exhaustion-downgraded-to-no-match', engine,
     '''        if self.inner.interrupted.load(Ordering::SeqCst) {
            return Err(MatchError::Interrupted);
        }''',
     '''        if self.inner.interrupted.load(Ordering::SeqCst) {
            return Ok(ExecOutcome::NoMatch);
        }''',
     None, 'budget_states_are_independently_distinguishable', 'budget',
     '#1265 budget exhaustion must be an explicit interrupt, not a verdict'),
    ('budget-unarmed-between-requests', engine,
     '''        self.inner.interrupted.store(false, Ordering::SeqCst);
        self.inner
            .remaining_polls
            .store(budget.remaining_polls, Ordering::SeqCst);''',
     '''        self.inner
            .remaining_polls
            .store(budget.remaining_polls, Ordering::SeqCst);''',
     None, 'fresh_budget_does_not_inherit_previous_exhaustion', 'budget',
     '#1265 a fresh budget must not inherit the previous request\'s exhaustion'),
    ('zero-budget-entry-removed', engine,
     '''        if budget.remaining_polls <= 0 {
            return Err(MatchError::Interrupted);
        }
''',
     '',
     None, 'zero_budget_refuses_to_start', 'budget',
     '#1265 a zero budget must not run even a sub-granularity subject'),
    ('cross-engine-check-removed', engine,
     '''        if !Arc::ptr_eq(&pattern.engine, &self.inner) {
            return Err(MatchError::Engine(
                "pattern belongs to another engine".to_owned(),
            ));
        }
''',
     '',
     None, 'exec_rejects_a_pattern_compiled_by_another_engine', 'lifecycle',
     ['#1265 a cross-engine exec must be rejected',
      '#1265 unexpected cross-engine error:']),
]

# The denominator of this run is a static literal (#1734): deleting, renaming
# or duplicating a row is a startup refusal, not a shorter green run.
EXPECTED_NAMES = (
    'interrupt-decrement-removed',
    'exhaustion-downgraded-to-no-match',
    'budget-unarmed-between-requests',
    'zero-budget-entry-removed',
    'cross-engine-check-removed',
)
try:
    registered(cases, EXPECTED_NAMES)
except ValueError as error:
    refuse(f'#1265 {error}')

args = sys.argv[1:]
if not args:
    refuse('usage: negative_controls.py OUT [mutation names]')
# The family target contract, not a local copy: absent/empty/blank targets and
# the dot/root/src values are refused by the shared rule, and every refusal
# is a "never started" (2) before the evidence directory is claimed
# (#1666 fix-r7/r9, #1734).
if missing_target():
    refuse('CARGO_TARGET_DIR must be set by the caller')
refuse_unusable_target()
# The shared selection, not a local copy: a repeated or unknown name refuses
# instead of shrinking the run (one valid name used to hide every typo,
# #1666 / #1734).
try:
    chosen = select(cases, args[1:], always=())
except ValueError as error:
    refuse(f'#1265 {error}')
env = os.environ.copy()
env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only',
           CARGO_PROFILE_TEST_DEBUG='line-tables-only', CARGO_TERM_QUIET='false')
out = claim_dir(args[0])


def run(label, binary, arguments):
    slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
    with (out / f'{label}.slots.log').open('x') as log:
        log.write(slot.stdout + slot.stderr)
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-regex-proto', '--features', 'regex-proto']
    if binary:
        command += ['--test', binary]
    result = run_owned(command + arguments, cwd=root, env=env, timeout=900)
    with (out / f'{label}.log').open('x') as log:
        log.write(result.stdout + result.stderr)
    return result


results = []
with mutating_sources(root, [engine]) as originals:
    # The engine's compile_fail doctests pin the !Send/!Sync contract but run
    # only under --features regex-proto (CI has no network for rquickjs), so
    # the fixture runs them itself, once, before its first mutation: a red
    # doctest baseline would otherwise let a mutation in that region go
    # unjudged. The run sits inside the guard's window (tree lock held,
    # bytes verified equal to HEAD): outside it, a concurrent instance of
    # this script could hold the lock with a mutant on disk and this run
    # would compile mutant bytes, not HEAD. This one refusal happens after
    # the claim, with the log kept in OUT.
    doctests = run('doctests', None, ['--doc'])
    if doctests.returncode:
        refuse('doctest invariant failed (see doctests.log in the evidence '
               'directory)')
    # rc 0 alone is not enough: a tree whose doctests were deleted or
    # renamed-to-ignore also exits 0, and the invariant would pass without
    # measuring anything (NOT_RUN as PASS). Both compile_fail doctests must
    # be counted on the result line.
    log = doctests.stdout + doctests.stderr
    counted = re.search(r'^test result: ok\. (\d+) passed;', log, re.M)
    measured = int(counted.group(1)) if counted else 0
    if measured < 2:
        refuse(f'doctest invariant measured {measured} passed doctests '
               '(expected the 2 compile_fail doctests; see doctests.log in '
               'the evidence directory)')
    for name, path, old, new, scope, test, binary, assertion in chosen:
        row = dict(mutation=name, test=test, assertion=assertion,
                   status='INVALID', reason='', compiled=False)
        try:
            changed = mutate(originals[path].decode(), scope, [(old, new)])
            if changed == originals[path].decode():
                raise ValueError('mutation leaves the source unchanged')
            if not passed(run(f'{name}-positive', binary,
                              [test, '--', '--exact']), test):
                raise RuntimeError('bound positive test failed')
            write_source_atomic(root, path, changed.encode())
            if run(f'{name}-compile', binary, ['--no-run']).returncode:
                raise RuntimeError('mutant compile failed')
            row['compiled'] = True
            # classify() reads .test/.assertion off its case argument.
            status, reason = classify(
                run(f'{name}-negative', binary, [test, '--', '--exact']),
                SimpleNamespace(test=test, assertion=assertion))
            row.update(status=status, reason=reason)
        except Exception as error:
            row.update(reason=f'{type(error).__name__}: {error}')
        finally:
            write_source_atomic(root, path, originals[path])
            if path.read_bytes() != originals[path]:
                raise RuntimeError(f'byte restoration failed: {path}')
        try:
            if not passed(run(f'{name}-restored', binary,
                              [test, '--', '--exact']), test):
                row.update(status='INVALID',
                           reason=row['reason'] + '; restored bound test did not pass')
        except Exception as error:
            row.update(status='INVALID', reason=row['reason'] + f'; restored: {error}')
        results.append(row)
        print(f"{name}: {row['status']}: {row['reason']}", flush=True)

# The summary is published only after the guard has restored and re-verified
# every byte, and atomically, through the shared publish_summary (#1579 r5:
# its pid+serial temp name cannot be clobbered by a concurrent writer the way
# the fixed local name could). The verdict is verdict_exit: at least one row
# and every row KILLED, so an empty run can never exit 0.
publish_summary(out, results)
print(json.dumps(pending.counts(results)), flush=True)
raise SystemExit(verdict_exit(results, key='status'))

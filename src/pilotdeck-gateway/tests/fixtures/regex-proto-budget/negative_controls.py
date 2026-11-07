#!/usr/bin/env python3
"""#1265 reverse negatives: the match budget must actually bound execution.

Three mutations of src/pilotdeck-regex-proto/src/engine.rs, each bound to one
exact test of pilotdeck-regex-proto/tests/budget.rs (which runs only under
--features regex-proto):

  interrupt-decrement-removed     the interrupt handler never charges a poll
                                  and never interrupts -> exhaustion test fails
  exhaustion-downgraded-to-no-match  a spent budget returns Ok(NoMatch) ->
                                  exhaustion test fails
  budget-unarmed-between-requests the interrupted flag is not cleared between
                                  requests -> the fresh-budget test fails

Commit first: mutating_sources proves engine.rs equals HEAD before anything is
written. Every mutant must pass `cargo test --no-run` before its negative run;
the bound test must pass before and after each mutation (restored bytes are
re-verified by the guard). All mutants run before the summary; any verdict
other than KILLED exits non-zero (verdict_exit: at least one row and all
KILLED). Mutants are published and restored with write_source_atomic (#1903),
never a truncating in-place write. Startup refusals -- unknown or duplicate
mutation names, a roster that no longer matches the static EXPECTED_NAMES
list, an unusable CARGO_TARGET_DIR (absent/empty/blank, or the dot/root/src
values the shared rule rejects), an already existing output directory --
all exit 2 ("never started", #1734), before the evidence directory is
claimed. The caller must set CARGO_TARGET_DIR (and the cargo slot gate's bin
dir on PATH); an expired anchor fails the row as INVALID, which also exits
non-zero.

Usage:
  negative_controls.py OUT [mutation names]
"""
import json
import os
from pathlib import Path
import sys
from types import SimpleNamespace

root = Path(__file__).resolve().parents[5]
fixtures = root / 'src/pilotdeck-gateway/tests/fixtures'
sys.path[:0] = [str(fixtures), str(fixtures / 'turn-runner')]
from negative_process import (mutating_sources, run_owned, wait_for_slot,
                              write_source_atomic)
from negative_selection import (claim_dir, missing_target, refuse,
                                refuse_unusable_target, registered,
                                verdict_exit)
from negative_suite import classify, passed
from _negative_common import mutate

engine = root / 'src/pilotdeck-regex-proto/src/engine.rs'
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
     None, 'budget_states_are_independently_distinguishable',
     '#1265 budget exhaustion must be an explicit interrupt, not a verdict'),
    ('exhaustion-downgraded-to-no-match', engine,
     '''        if self.inner.interrupted.load(Ordering::SeqCst) {
            return Err(MatchError::Interrupted);
        }''',
     '''        if self.inner.interrupted.load(Ordering::SeqCst) {
            return Ok(ExecOutcome::NoMatch);
        }''',
     None, 'budget_states_are_independently_distinguishable',
     '#1265 budget exhaustion must be an explicit interrupt, not a verdict'),
    ('budget-unarmed-between-requests', engine,
     '''        self.inner.interrupted.store(false, Ordering::SeqCst);
        self.inner
            .remaining_polls
            .store(budget.remaining_polls, Ordering::SeqCst);''',
     '''        self.inner
            .remaining_polls
            .store(budget.remaining_polls, Ordering::SeqCst);''',
     None, 'fresh_budget_does_not_inherit_previous_exhaustion',
     '#1265 a fresh budget must not inherit the previous request\'s exhaustion'),
]

# The denominator of this run is a static literal (#1734): deleting, renaming
# or duplicating a row is a startup refusal, not a shorter green run.
EXPECTED_NAMES = (
    'interrupt-decrement-removed',
    'exhaustion-downgraded-to-no-match',
    'budget-unarmed-between-requests',
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
names = args[1:]
if len(set(names)) != len(names):
    refuse('duplicate mutation names: ' + ', '.join(sorted(names)))
unknown = set(names) - set(EXPECTED_NAMES)
if unknown:
    refuse('unknown mutation: ' + ', '.join(sorted(unknown)))
chosen = [case for case in cases if not names or case[0] in names]
env = os.environ.copy()
env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only',
           CARGO_PROFILE_TEST_DEBUG='line-tables-only', CARGO_TERM_QUIET='false')
out = claim_dir(args[0])
command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
           '-p', 'pilotdeck-regex-proto', '--features', 'regex-proto',
           '--test', 'budget']


def run(label, test=None, compile_only=False):
    slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
    with (out / f'{label}.slots.log').open('x') as log:
        log.write(slot.stdout + slot.stderr)
    arguments = ['--no-run'] if compile_only else [test, '--', '--exact']
    result = run_owned(command + arguments, cwd=root, env=env, timeout=900)
    with (out / f'{label}.log').open('x') as log:
        log.write(result.stdout + result.stderr)
    return result


results = []
with mutating_sources(root, [engine]) as originals:
    for name, path, old, new, scope, test, assertion in chosen:
        row = dict(mutation=name, test=test, assertion=assertion,
                   status='INVALID', reason='', compiled=False)
        try:
            changed = mutate(originals[path].decode(), scope, [(old, new)])
            if changed == originals[path].decode():
                raise ValueError('mutation leaves the source unchanged')
            if not passed(run(f'{name}-positive', test), test):
                raise RuntimeError('bound positive test failed')
            write_source_atomic(root, path, changed.encode())
            if run(f'{name}-compile', compile_only=True).returncode:
                raise RuntimeError('mutant compile failed')
            row['compiled'] = True
            # classify() reads .test/.assertion off its case argument.
            status, reason = classify(
                run(f'{name}-negative', test),
                SimpleNamespace(test=test, assertion=assertion))
            row.update(status=status, reason=reason)
        except Exception as error:
            row.update(reason=f'{type(error).__name__}: {error}')
        finally:
            write_source_atomic(root, path, originals[path])
            if path.read_bytes() != originals[path]:
                raise RuntimeError(f'byte restoration failed: {path}')
        try:
            if not passed(run(f'{name}-restored', test), test):
                row.update(status='INVALID',
                           reason=row['reason'] + '; restored bound test did not pass')
        except Exception as error:
            row.update(status='INVALID', reason=row['reason'] + f'; restored: {error}')
        results.append(row)
        print(f"{name}: {row['status']}: {row['reason']}", flush=True)

# The summary is published only after the guard has restored and re-verified
# every byte, and atomically (tmp + rename): a cancelled window never pairs a
# stale or half-written all-green file with rc=0. The verdict is verdict_exit:
# at least one row and every row KILLED, so an empty run can never exit 0.
counts = {status: sum(row['status'] == status for row in results)
          for status in ('KILLED', 'SURVIVED', 'INVALID')}
summary_tmp = out / 'summary.json.tmp'
with summary_tmp.open('x') as summary:
    json.dump(dict(controls=results, counts=counts), summary, indent=2)
    summary.write('\n')
os.replace(summary_tmp, out / 'summary.json')
print(json.dumps(counts), flush=True)
raise SystemExit(verdict_exit(results, key='status'))

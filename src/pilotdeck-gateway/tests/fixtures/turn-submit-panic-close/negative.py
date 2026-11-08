#!/usr/bin/env python3
"""#2498 product mutations: every escape from the bounded panic-arm close must
redden `panicked_worker_closes_delivery_within_abort_budget`.

Each mutant reverts one slice of the #2498 fix (submit.rs, manage arm, worker
JoinError branch) and requires the named test to fail on its own `#2498`
assertion — never a hang, a compile error or a sibling test:

  panic-finish-unbounded     the pre-fix unbounded `publisher.finish().await`
                             (red cell: panic-close — appends settling after
                             the budget are delivered instead of dropped)
  panic-close-live-removed   drain kept, hard close dropped (red cell:
                             panic-close — the turn never ends without the
                             close, the bounded wire spin asserts)
  panic-drain-skipped        immediate close, no drain (red cell: panic-drain —
                             the in-flight failure frames are dropped)
  panic-budget-floor-removed budget without the `.max(1_000)` floor (red cell:
                             panic-budget — 20ms abort timeout closes at the
                             500ms step advance, frames dropped)

提交后由协调者在宿主跑：`mutating_sources` 证明整棵源码树等于 HEAD，而本
修复在提交前只存在于工作区——沙箱不能提交，本脚本随修复一起入库，协调者
提交后执行（每个变异体均已单独确认可编译、且手工注入时测试以 #2498 断言
红）。Run with no other writers/builds in this worktree; evidence goes in a new
output folder; original source bytes and hashes are verified after every
mutation.
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed
from negative_selection import (baseline_passed, claim_dir, missing_target, refuse,
                               refuse_unusable_target, registered, select,
                               verdict_exit)
from negative_process import (CARGO_SLOTS_WAIT_SECONDS, mutating_sources,
                              run_owned, wait_for_slot)

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime/turn'
# Each independent mutant is applied to the original, never cumulatively.
# The budget line alone also occurs in the timer-path backstop (submit.rs),
# so every anchor spans the panic arm's budget/timeout/close lines together.
mutations = [
    ('panic-finish-unbounded', 'submit.rs',
     '                        let budget = Duration::from_millis(self.host.abort_turn_timeout_ms().max(1_000));\n'
     '                        let _ = tokio::time::timeout(budget, publisher.finish()).await;\n'
     '                        publisher.close_live();',
     '                        publisher.finish().await;'),
    ('panic-close-live-removed', 'submit.rs',
     '                        let _ = tokio::time::timeout(budget, publisher.finish()).await;\n'
     '                        publisher.close_live();',
     '                        let _ = tokio::time::timeout(budget, publisher.finish()).await;'),
    ('panic-drain-skipped', 'submit.rs',
     '                        let budget = Duration::from_millis(self.host.abort_turn_timeout_ms().max(1_000));\n'
     '                        let _ = tokio::time::timeout(budget, publisher.finish()).await;\n'
     '                        publisher.close_live();',
     '                        publisher.close_live();'),
    ('panic-budget-floor-removed', 'submit.rs',
     '                        let budget = Duration::from_millis(self.host.abort_turn_timeout_ms().max(1_000));\n'
     '                        let _ = tokio::time::timeout(budget, publisher.finish()).await;',
     '                        let budget = Duration::from_millis(self.host.abort_turn_timeout_ms());\n'
     '                        let _ = tokio::time::timeout(budget, publisher.finish()).await;'),
]
# #1666: static denominator. `len(mutations)` is not a denominator: deleting a
# row shortens the run silently, and an emptied table prints `0/0 killed`.
EXPECTED_NAMES = (
    'panic-finish-unbounded', 'panic-close-live-removed',
    'panic-drain-skipped', 'panic-budget-floor-removed',
)
selected = sys.argv[2:]
# #1666: ordinary raises, so `python3 -O` cannot strip these guards.
try:
    registered(mutations, EXPECTED_NAMES)
    # `mutations` stays the frozen literal so `anchor_selfcheck.py` can read it
    # statically; the requested subset is a separate name.
    run_rows = select(mutations, selected, always=())
except ValueError as error:
    refuse(str(error))
env = os.environ.copy()
if missing_target(env):  # #1666: absent, empty or blank, one shared predicate
    refuse('CARGO_TARGET_DIR must be supplied by the caller')
refuse_unusable_target()  # #1666 fix-r9: dot / root / src are unusable too
out = claim_dir(sys.argv[1])
summary = []
failures = []

FULL_TEST = 'runtime::turn::submit::tests::panicked_worker_closes_delivery_within_abort_budget'

with mutating_sources(root, sorted({source / filename for _, filename, _, _ in run_rows})) as snapshots:
    def run(full):
        result = run_owned(['cargo','test','--manifest-path','src/Cargo.toml','-p','pilotdeck-gateway','--lib',
            full, '--', '--exact'], cwd=root, env=env, timeout=600)
        return result.returncode, result.stdout + result.stderr

    # #819: an unmutated baseline for the target must be green first.
    # #1908: the shared wait replaces the inline 300 s-sample /
    # 1800 s-deadline loop; the budget now comes from the one shared
    # constant that stays above the single-sample cap.
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=CARGO_SLOTS_WAIT_SECONDS)
    code, log = run(FULL_TEST)
    (out / f'baseline-{FULL_TEST.rsplit("::", 1)[-1]}.log').write_text(log)
    baseline_ok = code == 0 and baseline_passed(log, FULL_TEST)
    print(f'baseline {FULL_TEST}: {"PASS" if baseline_ok else "INVALID"}', flush=True)
    if not baseline_ok:
        failures.append('baseline')

    for name, filename, old, new in run_rows:
        path = source / filename
        original = snapshots[path]
        before = hashlib.sha256(original).hexdigest()
        content = original.decode()
        try:
            occurrences = content.count(old)
            if occurrences != 1:
                # A stale anchor is a hard error, never SURVIVED: the mutant
                # did not revert what it claims to revert.
                raise ValueError(
                    f'{name}: anchor occurs {occurrences} times (expected 1)')
            changed = content.replace(old, new, 1)
            (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(content.splitlines(True), changed.splitlines(True), fromfile=filename, tofile=filename)))
            # Required resource check; do not interpret the output as a concurrency
            # guarantee if the sandbox denies ps. Its raw output is retained.
            # #1908: same shared-wait migration as the baseline above.
            slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=CARGO_SLOTS_WAIT_SECONDS)
            (out / f'{name}.slots.log').write_text(slots.stdout + slots.stderr)
            path.write_text(changed)
            code, log = run(FULL_TEST)
            (out / f'{name}.log').write_text(log)
            # #819: the kill must be the named test's own #2498 assertion —
            # not stdout, a sibling test, a compile error or a watchdog hang.
            killed = (baseline_ok and code == 101
                      and named_test_killed(log, FULL_TEST, '#2498')
                      and 'could not compile' not in log)
            failure = next((line.strip() for line in log.splitlines()
                            if 'panicked at' in line or 'assertion' in line), '')
            status = 'KILLED' if killed else (
                'SURVIVED' if code == 0 else 'INVALID')
            summary.append(dict(name=name, test=FULL_TEST, status=status,
                                returncode=code, failure=failure,
                                sourceSha256=before))
            print(f'{name}: {status} rc={code} {failure}', flush=True)
            if status != 'KILLED':
                failures.append(name)
        except Exception as error:
            summary.append(dict(name=name, test=FULL_TEST, status='INVALID',
                                reason=f'{type(error).__name__}: {error}',
                                sourceSha256=before))
            failures.append(name)
        finally:
            path.write_bytes(original)
            restored = hashlib.sha256(path.read_bytes()).hexdigest()
            if restored != before:
                raise RuntimeError(
                    f'{name}: restore failed ({restored} != {before}); the tree is dirty')
            (out / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2)+'\n')
counts = {state: sum(row['status'] == state for row in summary)
          for state in ('KILLED', 'SURVIVED', 'INVALID')}
print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
if failures:
    print(f'not killed: {", ".join(failures)}', file=sys.stderr)
killed_rows = sum(1 for row in summary if row['status'] == 'KILLED')
print(f'{killed_rows}/{len(run_rows)} killed; every source restored byte-for-byte', flush=True)
# #1666: a zero-row run is never success (`0/0 killed` used to exit 0).
sys.exit(verdict_exit(summary, key='status'))

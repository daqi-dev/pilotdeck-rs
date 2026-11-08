#!/usr/bin/env python3
"""#2498 fix-r1 product mutations: every escape from the bounded panic-arm
close must redden its own named test.

Each mutant reverts one slice of the #2498 fix (submit.rs, manage arm, worker
JoinError branch — plus the publish.rs hard-close-aware trailing wait) and
requires its bound test to fail on its own `#2498` assertion — never a hang, a
compile error or a sibling test:

  panic-finish-unbounded     the drain loses its abort budget (bound:
                             panicked_worker_closes_delivery_within_abort_
                             budget — red cell: panic-close, the closure must
                             be observed while the failure appends are still
                             gated)
  panic-record-unbounded      the durable failure record moves back ahead of
                             the budget (bound: stuck_failure_record_still_
                             closes_the_panic_turn — the stuck record hangs
                             the turn before the close even starts)
  panic-close-live-removed   drain kept, hard close dropped (bound:
                             never_settling_appends_still_close_the_panic_
                             turn — without the close nothing ever ends the
                             stuck turn)
  panic-drain-skipped        record kept, drain dropped (bound:
                             panicked_worker_closes_delivery_within_abort_
                             budget — red cell: panic-drain, the in-flight
                             failure frames are dropped)
  panic-budget-floor-removed abort_budget() without the `.max(1_000)` floor
                             (bound: panicked_worker_closes_delivery_within_
                             abort_budget — red cell: panic-budget, the 20ms
                             configuration closes at the 500ms step advance)
  panic-trailing-wait-removed next()'s trailing wait reverts to the session-end
                             drain barrier `wait_for` (bound:
                             never_settling_appends_still_close_the_panic_
                             turn — the P0 shape: the consumer parks on the
                             pending set past the hard close)

提交后由协调者在宿主跑：`mutating_sources` 证明整棵源码树等于 HEAD，而本
修复在提交前只存在于工作区——沙箱不能提交，本脚本随修复一起入库，协调者
提交后执行（每个变异体均已单独确认可编译、且手工注入时绑定测试以 #2498
断言红）。Run with no other writers/builds in this worktree; evidence goes in
a new output folder; original source bytes and hashes are verified after every
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
# The panic arm's budget/timeout/close lines span one contiguous block, so
# every submit.rs anchor covers the whole arm; the floor lives in the shared
# abort_budget() helper (the bare call sites occur twice and cannot anchor),
# and the trailing-wait revert anchors on next()'s #2498 comment plus call.
SUBMIT_TEST = 'runtime::turn::submit::tests::panicked_worker_closes_delivery_within_abort_budget'
STUCK_TEST = 'runtime::turn::submit::tests::never_settling_appends_still_close_the_panic_turn'
RECORD_TEST = 'runtime::turn::submit::tests::stuck_failure_record_still_closes_the_panic_turn'
mutations = [
    ('panic-finish-unbounded', 'submit.rs',
     '                        let owned = terminal.claim();\n'
     '                        let budget = self.abort_budget();\n'
     '                        let _ = tokio::time::timeout(budget, async {\n'
     '                            if owned {\n'
     '                                self.publish_failure(&run, &publisher, &SubmitError::new("gateway_submit_failed", error.to_string())).await;\n'
     '                            }\n'
     '                            publisher.finish().await;\n'
     '                        })\n'
     '                        .await;\n'
     '                        publisher.close_live();',
     '                        let owned = terminal.claim();\n'
     '                        let budget = self.abort_budget();\n'
     '                        let _ = budget;\n'
     '                        if owned {\n'
     '                            self.publish_failure(&run, &publisher, &SubmitError::new("gateway_submit_failed", error.to_string())).await;\n'
     '                        }\n'
     '                        publisher.finish().await;\n'
     '                        publisher.close_live();',
     SUBMIT_TEST),
    ('panic-record-unbounded', 'submit.rs',
     '                        let owned = terminal.claim();\n'
     '                        let budget = self.abort_budget();\n'
     '                        let _ = tokio::time::timeout(budget, async {\n'
     '                            if owned {\n'
     '                                self.publish_failure(&run, &publisher, &SubmitError::new("gateway_submit_failed", error.to_string())).await;\n'
     '                            }\n'
     '                            publisher.finish().await;\n'
     '                        })\n'
     '                        .await;\n'
     '                        publisher.close_live();',
     '                        let owned = terminal.claim();\n'
     '                        if owned {\n'
     '                            self.publish_failure(&run, &publisher, &SubmitError::new("gateway_submit_failed", error.to_string())).await;\n'
     '                        }\n'
     '                        let budget = self.abort_budget();\n'
     '                        let _ = tokio::time::timeout(budget, async {\n'
     '                            publisher.finish().await;\n'
     '                        })\n'
     '                        .await;\n'
     '                        publisher.close_live();',
     RECORD_TEST),
    ('panic-close-live-removed', 'submit.rs',
     '                        let _ = tokio::time::timeout(budget, async {\n'
     '                            if owned {\n'
     '                                self.publish_failure(&run, &publisher, &SubmitError::new("gateway_submit_failed", error.to_string())).await;\n'
     '                            }\n'
     '                            publisher.finish().await;\n'
     '                        })\n'
     '                        .await;\n'
     '                        publisher.close_live();',
     '                        let _ = tokio::time::timeout(budget, async {\n'
     '                            if owned {\n'
     '                                self.publish_failure(&run, &publisher, &SubmitError::new("gateway_submit_failed", error.to_string())).await;\n'
     '                            }\n'
     '                            publisher.finish().await;\n'
     '                        })\n'
     '                        .await;',
     STUCK_TEST),
    ('panic-drain-skipped', 'submit.rs',
     '                        let owned = terminal.claim();\n'
     '                        let budget = self.abort_budget();\n'
     '                        let _ = tokio::time::timeout(budget, async {\n'
     '                            if owned {\n'
     '                                self.publish_failure(&run, &publisher, &SubmitError::new("gateway_submit_failed", error.to_string())).await;\n'
     '                            }\n'
     '                            publisher.finish().await;\n'
     '                        })\n'
     '                        .await;\n'
     '                        publisher.close_live();',
     '                        let owned = terminal.claim();\n'
     '                        let budget = self.abort_budget();\n'
     '                        let _ = budget;\n'
     '                        if owned {\n'
     '                            self.publish_failure(&run, &publisher, &SubmitError::new("gateway_submit_failed", error.to_string())).await;\n'
     '                        }\n'
     '                        publisher.close_live();',
     SUBMIT_TEST),
    ('panic-budget-floor-removed', 'submit.rs',
     '    fn abort_budget(&self) -> Duration {\n'
     '        Duration::from_millis(self.host.abort_turn_timeout_ms().max(1_000))\n'
     '    }',
     '    fn abort_budget(&self) -> Duration {\n'
     '        Duration::from_millis(self.host.abort_turn_timeout_ms())\n'
     '    }',
     SUBMIT_TEST),
    ('panic-trailing-wait-removed', 'publish.rs',
     '                // #2498: the trailing wait ends at a hard close — a\n'
     '                // never-settling append must not park the consumer past\n'
     '                // `close_live`, or `join!(manage, consume)` in submit never\n'
     '                // returns and the turn, admission and completion fence leak.\n'
     '                // Frames already in the queue above were settled before the\n'
     '                // close and are still drained first, like TS\'s `queue.close`.\n'
     '                self.publication.wait_for_trailing_writes(&writes).await;',
     '                self.publication.wait_for(&writes).await;',
     STUCK_TEST),
]
# #1666: static denominator. `len(mutations)` is not a denominator: deleting a
# row shortens the run silently, and an emptied table prints `0/0 killed`.
EXPECTED_NAMES = (
    'panic-finish-unbounded', 'panic-record-unbounded',
    'panic-close-live-removed', 'panic-drain-skipped',
    'panic-budget-floor-removed', 'panic-trailing-wait-removed',
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

# #2498 fix-r1: every mutant names the test that must redden for it; the
# unmutated baseline covers each of them, once, before any mutation runs.
bound_tests = list(dict.fromkeys(row[4] for row in run_rows))

with mutating_sources(root, sorted({source / filename for _, filename, _, _, _ in run_rows})) as snapshots:
    def run(full):
        result = run_owned(['cargo','test','--manifest-path','src/Cargo.toml','-p','pilotdeck-gateway','--lib',
            full, '--', '--exact'], cwd=root, env=env, timeout=600)
        return result.returncode, result.stdout + result.stderr

    # #819: an unmutated baseline for every bound test must be green first.
    # #1908: the shared wait replaces the inline 300 s-sample /
    # 1800 s-deadline loop; the budget now comes from the one shared
    # constant that stays above the single-sample cap.
    for bound in bound_tests:
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=CARGO_SLOTS_WAIT_SECONDS)
        code, log = run(bound)
        (out / f'baseline-{bound.rsplit("::", 1)[-1]}.log').write_text(log)
        baseline_ok = code == 0 and baseline_passed(log, bound)
        print(f'baseline {bound}: {"PASS" if baseline_ok else "INVALID"}', flush=True)
        if not baseline_ok:
            failures.append(f'baseline {bound}')

    for name, filename, old, new, bound in run_rows:
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
            code, log = run(bound)
            (out / f'{name}.log').write_text(log)
            # #819: the kill must be the bound test's own #2498 assertion —
            # not stdout, a sibling test, a compile error or a watchdog hang.
            killed = (code == 101
                      and named_test_killed(log, bound, '#2498')
                      and 'could not compile' not in log)
            failure = next((line.strip() for line in log.splitlines()
                            if 'panicked at' in line or 'assertion' in line), '')
            status = 'KILLED' if killed else (
                'SURVIVED' if code == 0 else 'INVALID')
            summary.append(dict(name=name, test=bound, status=status,
                                returncode=code, failure=failure,
                                sourceSha256=before))
            print(f'{name}: {status} rc={code} {failure}', flush=True)
            if status != 'KILLED':
                failures.append(name)
        except Exception as error:
            summary.append(dict(name=name, test=bound, status='INVALID',
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

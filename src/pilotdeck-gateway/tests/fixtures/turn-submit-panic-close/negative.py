#!/usr/bin/env python3
"""#2498 fix-r1/fix-r2 product mutations: every escape from the bounded
panic-arm and timer-arm closes must redden its own named test.

Each mutant reverts one slice of the #2498 fix (submit.rs, manage arm, worker
JoinError branch and timer-arm record — plus the publish.rs hard-close-aware
trailing wait) and requires its bound test to fail on its own `#2498`
assertion — never a hang, a compile error or a sibling test:

  panic-finish-unbounded     the drain loses its abort budget (bound:
                             panicked_worker_closes_delivery_within_abort_
                             budget — red cell: panic-close, the closure must
                             be observed while the failure appends are still
                             gated)
  panic-record-unbounded      the durable failure record moves back ahead of
                             the budget (bound: stuck_failure_record_still_
                             closes_the_panic_turn — the stuck record hangs
                             the turn before the close even starts)
  panic-record-unbounded-slow-cell  the same revert pinned to the third cell
                             the shared budget introduces (bound:
                             slow_failure_record_settling_after_budget_drops_
                             the_failure_frames — the record settles after
                             the budget, so the close must happen while it is
                             still gated and its late frames must not
                             enqueue)
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
  timer-record-unbounded      the timer winner's durable record moves back out
                             of the budget (bound: stuck_timer_record_still_
                             closes_the_turn — the stuck record parks the
                             manage arm ahead of the settlement and the
                             close, the same hang shape the panic arm had
                             before fix-r1)

#2498 fix-r2 (P0): a kill requires the bound test's own green baseline — the
restored `bound in baseline_ok` gate (dropped in the fix-r1 per-bound-test
refactor; a red unmutated bound test still counted as KILLED and let the
apparatus exit 0). The broken-baseline self-check below breaks one baseline
on purpose: the red log must satisfy every other kill condition (the probe
has teeth — the ungated predicate would call it KILLED) while the gated
verdict, run against that broken baseline, must refuse the kill.

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
                              run_owned, wait_for_slot, write_source_atomic)

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
SLOW_TEST = 'runtime::turn::submit::tests::slow_failure_record_settling_after_budget_drops_the_failure_frames'
TIMER_TEST = 'runtime::turn::submit::tests::stuck_timer_record_still_closes_the_turn'
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
    ('panic-record-unbounded-slow-cell', 'submit.rs',
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
     SLOW_TEST),
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
    ('timer-record-unbounded', 'submit.rs',
     '                        let budget = self.abort_budget();\n'
     '                        let _ = tokio::time::timeout(budget, async {\n'
     '                            if self.host.record_failure(&run, &record).await.is_ok() {\n'
     '                                publisher.gateway_event(with_run(failure, &run));\n'
     '                            }\n'
     '                            let mut event = error_event(&run, "turn_timeout", &message, false);\n'
     '                            set(&mut event, "userHint", hint.into());\n'
     '                            publisher.gateway_event(event);\n'
     '                        })\n'
     '                        .await;',
     '                        if self.host.record_failure(&run, &record).await.is_ok() {\n'
     '                            publisher.gateway_event(with_run(failure, &run));\n'
     '                        }\n'
     '                        let mut event = error_event(&run, "turn_timeout", &message, false);\n'
     '                        set(&mut event, "userHint", hint.into());\n'
     '                        publisher.gateway_event(event);',
     TIMER_TEST),
]
# #1666: static denominator. `len(mutations)` is not a denominator: deleting a
# row shortens the run silently, and an emptied table prints `0/0 killed`.
EXPECTED_NAMES = (
    'panic-finish-unbounded', 'panic-record-unbounded',
    'panic-record-unbounded-slow-cell', 'panic-close-live-removed',
    'panic-drain-skipped', 'panic-budget-floor-removed',
    'panic-trailing-wait-removed', 'timer-record-unbounded',
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

# #2498 fix-r2: the timer-arm mutation and the broken-baseline self-check
# below touch submit.rs and submit_tests.rs too, so both join the guarded set.
guarded = sorted({source / filename for _, filename, _, _, _ in run_rows}
                 | {source / 'submit_tests.rs'})
with mutating_sources(root, guarded) as snapshots:
    # #2498 fix-r3: every write-back below (self-check and mutation loop)
    # publishes through the shared atomic rename — the same protocol
    # `mutating_sources`' own restore uses. `write_text` / `write_bytes`
    # truncate in place, so a SIGKILL mid-write-back leaves a half-written
    # source and the whole crate stops compiling. The mode is captured while
    # the files are pristine, exactly when `mutating_sources` snapshots it.
    source_modes = {path: path.stat().st_mode & 0o7777 for path in guarded}
    def run(full):
        result = run_owned(['cargo','test','--manifest-path','src/Cargo.toml','-p','pilotdeck-gateway','--lib',
            full, '--', '--exact'], cwd=root, env=env, timeout=600)
        return result.returncode, result.stdout + result.stderr

    def kill_verdict(bound, code, log, green):
        # #2498 fix-r2 (P0): the restored fix-r0 gate — a kill requires the
        # bound test's own green baseline. A red unmutated bound test must
        # never count as a kill, no matter how `#2498`-shaped its panic is.
        return (bound in green
                and code == 101
                and named_test_killed(log, bound, '#2498')
                and 'could not compile' not in log)

    # #819: an unmutated baseline for every bound test must be green first.
    # #1908: the shared wait replaces the inline 300 s-sample /
    # 1800 s-deadline loop; the budget now comes from the one shared
    # constant that stays above the single-sample cap.
    # #2498 fix-r2 (P0): `baseline_ok` is the set of bound tests proven
    # green above — the same shape as the sibling fixtures (interaction-bus,
    # turn-publish). The fix-r1 per-bound-test refactor dropped the scalar
    # gate's last-wins successor entirely; the set restores it per test.
    baseline_ok = set()
    for bound in bound_tests:
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=CARGO_SLOTS_WAIT_SECONDS)
        code, log = run(bound)
        (out / f'baseline-{bound.rsplit("::", 1)[-1]}.log').write_text(log)
        if code == 0 and baseline_passed(log, bound):
            baseline_ok.add(bound)
        print(f'baseline {bound}: {"PASS" if bound in baseline_ok else "INVALID"}', flush=True)
        if bound not in baseline_ok:
            failures.append(f'baseline {bound}')

    # #2498 fix-r2 (P0) negative self-check: the gate must actually gate.
    # Break one baseline on purpose — inject a `#2498`-marked panic into the
    # bound test's own body — then, with the product unmutated, the red log
    # must satisfy every kill condition except the baseline (the exact shape
    # that, with the gate dropped again, counted as a false KILLED and let a
    # broken apparatus exit 0), and the baseline computed from that same log
    # must be False. End-to-end, a real mutation run against that broken
    # baseline must not be KILLED either — the gate refuses the pin.
    tests_path = source / 'submit_tests.rs'
    tests_original = snapshots[tests_path]
    tests_before = hashlib.sha256(tests_original).hexdigest()
    tests_content = tests_original.decode()
    selfcheck_anchor = 'async fn panicked_worker_closes_delivery_within_abort_budget() {'
    selfcheck_hits = tests_content.count(selfcheck_anchor)
    if selfcheck_hits != 1:
        raise ValueError(
            f'broken-baseline selfcheck: anchor occurs {selfcheck_hits} times (expected 1)')
    broken_tests = tests_content.replace(
        selfcheck_anchor,
        selfcheck_anchor
        + '\n    panic!("#2498 broken-baseline selfcheck: a red baseline must not gate a kill");',
        1)
    selfcheck_row = next(
        (row for row in run_rows if row[0] == 'panic-finish-unbounded'), None)
    if selfcheck_row is None:
        # A subset without its pin cannot self-check the gate; run the full
        # table (or include panic-finish-unbounded) for the self-check.
        # #2498 fix-r3: refuse (rc=2, "never started"), never a traceback
        # exit 1 — rc 1 reads as "ran and was not green", i.e. a surviving
        # mutant, the false-FAIL direction of the audit.
        refuse('broken-baseline selfcheck: panic-finish-unbounded is not selected')
    _, selfcheck_file, selfcheck_old, selfcheck_new, _ = selfcheck_row
    selfcheck_path = source / selfcheck_file
    selfcheck_original = snapshots[selfcheck_path]
    selfcheck_before = hashlib.sha256(selfcheck_original).hexdigest()
    selfcheck_content = selfcheck_original.decode()
    if selfcheck_content.count(selfcheck_old) != 1:
        raise ValueError('broken-baseline selfcheck: mutant anchor is not unique')
    try:
        (out / 'broken-baseline-tests.patch').write_text(''.join(difflib.unified_diff(
            tests_content.splitlines(True), broken_tests.splitlines(True),
            fromfile='submit_tests.rs', tofile='submit_tests.rs')))
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=CARGO_SLOTS_WAIT_SECONDS)
        (out / 'broken-baseline-tests.slots.log').write_text(slots.stdout + slots.stderr)
        write_source_atomic(root, tests_path, broken_tests.encode(), mode=source_modes[tests_path])
        code, log = run(SUBMIT_TEST)
        (out / 'broken-baseline-unmutated.log').write_text(log)
        # Teeth: with the gate bypassed (a pretend-green baseline), the red
        # log satisfies every other kill condition — without `bound in
        # baseline_ok` this state is the false KILLED.
        teeth = kill_verdict(SUBMIT_TEST, code, log, {SUBMIT_TEST})
        # The baseline rule applied to that same log refuses the test.
        broken_green = ({SUBMIT_TEST}
                        if code == 0 and baseline_passed(log, SUBMIT_TEST)
                        else set())
        # End-to-end: the real mutation, still against the broken baseline,
        # must not be KILLED. #2498 fix-r3: this second cargo run waits for
        # its slot like every other invocation (it used to be the one call
        # outside the shared quota).
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=CARGO_SLOTS_WAIT_SECONDS)
        (out / 'broken-baseline-mutated.slots.log').write_text(slots.stdout + slots.stderr)
        write_source_atomic(root, selfcheck_path,
                             selfcheck_content.replace(selfcheck_old, selfcheck_new, 1).encode(),
                             mode=source_modes[selfcheck_path])
        try:
            code, log = run(SUBMIT_TEST)
            (out / 'broken-baseline-mutated.log').write_text(log)
            refused = not kill_verdict(SUBMIT_TEST, code, log, broken_green)
        finally:
            write_source_atomic(root, selfcheck_path, selfcheck_original,
                                mode=source_modes[selfcheck_path])
            restored = hashlib.sha256(selfcheck_path.read_bytes()).hexdigest()
            if restored != selfcheck_before:
                raise RuntimeError(
                    f'broken-baseline selfcheck: restore failed ({restored} != {selfcheck_before}); the tree is dirty')
        selfcheck_ok = teeth and not broken_green and refused
        print(f'broken-baseline selfcheck: {"PASS" if selfcheck_ok else "INVALID"}'
              f' (teeth={teeth}, baseline={bool(broken_green)}, kill refused={refused})', flush=True)
        if not selfcheck_ok:
            failures.append('broken-baseline selfcheck')
    finally:
        write_source_atomic(root, tests_path, tests_original,
                            mode=source_modes[tests_path])
        restored = hashlib.sha256(tests_path.read_bytes()).hexdigest()
        if restored != tests_before:
            raise RuntimeError(
                f'broken-baseline selfcheck: restore failed ({restored} != {tests_before}); the tree is dirty')

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
            write_source_atomic(root, path, changed.encode(), mode=source_modes[path])
            code, log = run(bound)
            (out / f'{name}.log').write_text(log)
            # #819: the kill must be the bound test's own #2498 assertion —
            # not stdout, a sibling test, a compile error or a watchdog hang.
            # #2498 fix-r2 (P0): and only with the bound test's own green
            # baseline (the gate the fix-r1 refactor dropped).
            killed = kill_verdict(bound, code, log, baseline_ok)
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
            write_source_atomic(root, path, original, mode=source_modes[path])
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
# #2498 fix-r3: a broken-baseline self-check (or a red baseline) used to be
# print-only — `verdict_exit` sees only the mutation rows, so an INVALID
# self-check still exited 0, the fix-r1 false-green shape again. The sibling
# close (turn-publish/negative.py:169-175): `failures` must reach the exit
# code. Chosen over folding the self-check into `summary` because a refused
# self-check is a failure of the apparatus, not a mutation verdict row.
if failures:
    sys.exit(1)
# #1666: a zero-row run is never success (`0/0 killed` used to exit 0).
sys.exit(verdict_exit(summary, key='status'))

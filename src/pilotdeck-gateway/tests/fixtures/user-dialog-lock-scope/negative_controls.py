#!/usr/bin/env python3
"""Temporary product mutations; require an oracle assertion failure, then restore.

#1442/#1599/#1600 negative controls: each mutation puts one host-supplied
callback (the resolver, the settle hook, the accepts validator, or the change
observer) back under the GatewayUserDialogBus MutexGuard, drifts the shared
`management_tests::setup` fixture back to the retired unconnected bus, or
drops the identity binding that keeps an answer bound to the dialog it was
validated against. The matching probe test must turn red. Every mutant keeps
the callback call count and order of the original code, so the kill evidence
is the "runs under the guard" probe rather than a duplicated callback. Run
with no other writers/builds in this worktree. Evidence goes in a new output
folder; original source bytes and hashes are verified after every mutation.

Phases per mutation (specs/negative-controls/README.md): a green baseline for
the bound test (positive) -> the mutant must die at its own assertion
(negative) -> the restored bytes must run green again (restored). Any phase
failing records INVALID, every other mutation still runs.

The declared denominator [`DECLARED`] is a literal in this file, never the
runtime table: emptying or trimming `mutations` is drift, and drift publishes
a suite-level INVALID row and exits non-zero (the empty-table clause of the
negative-control contract, #996 fix-r6). An explicit argv subset is a user
request and stays green when every selected row is KILLED.

Remote-host mode keeps the delivery uncommitted, so the baseline is supplied
by the caller (argv[2]): a `git stash create` commit of the staged delivery
tree, never a pushed ref.
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_failed, panic_evidence
from negative_selection import baseline_passed
from negative_process import mutating_sources, run_owned, wait_for_slot as gate_wait_for_slot

root = Path(__file__).resolve().parents[5]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
baseline = sys.argv[2]
source = root / 'src/pilotdeck-gateway/src'
LOCK_ASSERTION = 'must not run under the guard'
FIXTURE_ASSERTION = "the shared fixture's user-dialog bus must be connected"
SETTLE_TEST = ('native_process::methods::tests::'
               'user_dialog_settle_and_resolve_reenter_the_bus_lock_free')
OBSERVER_TEST = ('native_process::methods::tests::'
                 'user_dialog_observer_reenters_the_bus_lock_free')
ACCEPTS_TEST = ('native_process::methods::tests::'
                'user_dialog_accepts_reenters_the_bus_lock_free')
FIXTURE_TEST = ('native_process::management_tests::'
                'setup_assembles_a_connected_user_dialog_bus')
CHANNEL_TEST = ('user_dialog::channel_tests::'
                'user_dialog_channel_cancel_reenters_the_bus_lock_free')
CROSS_TEST = ('user_dialog::channel_tests::'
              'cross_gateway_answer_is_not_delivered_to_a_dialog_replaced_during_the_take')
CROSS_ASSERTION = ('#1599 a cross-gateway answer must not be delivered to a '
                   'dialog replaced during take_stored_answer')
REPLACED_TEST = ('user_dialog::tests::'
                 'consume_for_response_refuses_a_dialog_replaced_after_the_snapshot')
REPLACED_ASSERTION = ('#1599 an answer validated against a replaced dialog '
                      'must not be delivered')
RESTORE_TEST = ('user_dialog::tests::'
                'restore_keeps_the_consumed_dialog_at_its_original_position')
RESTORE_ASSERTION = ('#1599 restore puts the consumed dialog back at its '
                     'original position')
# Statically declared denominator: a literal tuple, deliberately not derived
# from `mutations`, so a runtime table that lost rows cannot shrink it.
DECLARED = (
    'resolve-back-under-guard',
    'settle-hook-back-under-guard',
    'accepts-back-under-guard',
    'observer-delivery-back-under-guard',
    'fixture-drifts-to-unconnected-bus',
    'channel-cancel-settle-back-under-guard',
    'replaced-dialog-still-consumed',
    'restore-appends-to-the-tail',
    'cross-gateway-replaced-dialog-still-consumed',
)
# Each independent mutant is applied to the original, never cumulatively.
mutations = [
    # respond: the resolver runs while a fresh guard is alive again.
    ('resolve-back-under-guard', 'native_process/methods/user_dialog.rs',
     '                            outbox.deliver();\n'
     '                            (consumed.entry.resolve)(answer);',
     '                            outbox.deliver();\n'
     '                            let _guard = self\n'
     '                                .user_dialog_bus\n'
     '                                .lock()\n'
     '                                .unwrap_or_else(|e| e.into_inner());\n'
     '                            (consumed.entry.resolve)(answer);',
     SETTLE_TEST, LOCK_ASSERTION),
    # consume: the settle hook runs with a guard alive again (called once).
    ('settle-hook-back-under-guard', 'native_process/methods/user_dialog.rs',
     '                            let settled = consumed\n'
     '                                .entry\n'
     '                                .on_settled\n'
     '                                .as_ref()\n'
     '                                .map_or(Ok(()), |hook| hook());',
     '                            let settled = {\n'
     '                                let _guard = self\n'
     '                                    .user_dialog_bus\n'
     '                                    .lock()\n'
     '                                    .unwrap_or_else(|e| e.into_inner());\n'
     '                                consumed\n'
     '                                    .entry\n'
     '                                    .on_settled\n'
     '                                    .as_ref()\n'
     '                                    .map_or(Ok(()), |hook| hook())\n'
     '                            };',
     SETTLE_TEST, LOCK_ASSERTION),
    # respond: the accepts validator runs with a guard alive again (called once).
    ('accepts-back-under-guard', 'native_process/methods/user_dialog.rs',
     '                        if !(snapshot.accepts)(&value) {',
     '                        let accepted = {\n'
     '                            let _guard = self\n'
     '                                .user_dialog_bus\n'
     '                                .lock()\n'
     '                                .unwrap_or_else(|e| e.into_inner());\n'
     '                            (snapshot.accepts)(&value)\n'
     '                        };\n'
     '                        if !accepted {',
     ACCEPTS_TEST, LOCK_ASSERTION),
    # emit: the observer is invoked in place instead of being queued.
    ('observer-delivery-back-under-guard', 'user_dialog/bus.rs',
     '    fn emit(&mut self, change: JsValue) {\n'
     '        self.outbox.push(change);\n'
     '    }',
     '    fn emit(&mut self, change: JsValue) {\n'
     '        let observer = self.on_change.clone();\n'
     '        if let Some(observer) = observer {\n'
     '            let _ = observer(change);\n'
     '        }\n'
     '    }',
     OBSERVER_TEST, LOCK_ASSERTION),
    # #1600: the shared fixture drifts back to the retired unconnected bus.
    ('fixture-drifts-to-unconnected-bus', 'native_process/management_tests.rs',
     '            user_dialog_bus: Arc::new(std::sync::Mutex::new(\n'
     '                crate::user_dialog::GatewayUserDialogBus::connected(\n'
     '                    crate::user_dialog::DialogRuntime::default(),\n'
     '                ),\n'
     '            )),',
     '            user_dialog_bus: Arc::new(std::sync::Mutex::new(\n'
     '                crate::user_dialog::GatewayUserDialogBus::new(\n'
     '                    crate::user_dialog::DialogRuntime::default(),\n'
     '                    None,\n'
     '                ),\n'
     '            )),',
     FIXTURE_TEST, FIXTURE_ASSERTION),
    # channel.cancel: the settle hook runs with a guard alive again.
    ('channel-cancel-settle-back-under-guard', 'user_dialog/channel.rs',
     '            Some(consumed) => {\n'
     '                let settled = consumed\n'
     '                    .entry\n'
     '                    .on_settled\n'
     '                    .as_ref()\n'
     '                    .map_or(Ok(()), |hook| hook());',
     '            Some(consumed) => {\n'
     '                let settled = {\n'
     '                    let _guard = self.inner.options.bus.lock().unwrap();\n'
     '                    consumed\n'
     '                        .entry\n'
     '                        .on_settled\n'
     '                        .as_ref()\n'
     '                        .map_or(Ok(()), |hook| hook())\n'
     '                };',
     CHANNEL_TEST, LOCK_ASSERTION),
    # consume_for_response: the snapshot identity check is dropped, so an
    # answer validated against a replaced dialog reaches the new one.
    ('replaced-dialog-still-consumed', 'user_dialog/bus.rs',
     '        if validated.is_some_and(|validated| !Arc::ptr_eq(&pending.accepts, validated)) {',
     '        if validated.is_some_and(|_validated| false) {',
     REPLACED_TEST, REPLACED_ASSERTION),
    # restore: the entry goes back to the bucket tail instead of its position.
    ('restore-appends-to-the-tail', 'user_dialog/bus.rs',
     '            None => bucket.insert(index.min(bucket.len()), entry),',
     '            None => bucket.push(entry),',
     RESTORE_TEST, RESTORE_ASSERTION),
    # channel cross-gateway poll: the identity check is dropped, so an answer
    # taken for one dialog is consumed from whatever is pending under the id.
    ('cross-gateway-replaced-dialog-still-consumed', 'user_dialog/channel.rs',
     '            let bound =\n'
     '                bus.pending_matches(&self.inner.options.session_key, request_id, &state.accepts);',
     '            let bound = bus\n'
     '                .peek(&self.inner.options.session_key, request_id)\n'
     '                .is_some();',
     CROSS_TEST, CROSS_ASSERTION),
]
declared = list(DECLARED)
selected = sys.argv[3:]
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR'):
    sys.exit('CARGO_TARGET_DIR must be supplied by the caller')
summary = []
failures = []


def wait_for_slot(name):
    # Single source of truth for the gate contract (retries inside one budget,
    # no bare subprocess timeout); its raw output is retained per mutation.
    slot = gate_wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=1800, interval=5)
    (out / f'{name}.slots.log').write_text(slot.stdout + slot.stderr)


def say(message):
    # A closed pipe must not turn one result row into two (the print sits inside
    # the per-mutation try, so an unguarded BrokenPipeError lands in its except).
    try:
        print(message, flush=True)
    except OSError:
        pass


def publish_summary():
    # Publishes once, after the source guard has restored and verified the bytes
    # (specs/negative-controls/README.md). `total` is the statically declared
    # denominator, never the argv selection, so a subset run and a full run stay
    # distinguishable in the evidence. Rows live under `controls` like the
    # shared `negative_suite.publish_summary`.
    counts = {state: sum(row['status'] == state for row in summary)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    payload = dict(total=len(declared), declared=declared,
                   selected=[row['name'] for row in summary],
                   counts=counts, controls=summary)
    tmp = out / 'summary.json.tmp'
    tmp.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + '\n')
    os.replace(tmp, out / 'summary.json')
    return counts


def suite_row(reason):
    # Denominator drift has no per-mutation row to mark, so it gets its own
    # suite-level INVALID row; otherwise an empty run would publish green.
    summary.append(dict(name='switch-denominator', test=None, assertion=None,
                        status='INVALID', returncode=None, reason=reason,
                        sourceSha256=None))
    failures.append('switch-denominator')


runtime_names = [row[0] for row in mutations]
drift = ([name for name in runtime_names if name not in declared]
         + [name for name in declared if name not in runtime_names])
unknown = [name for name in selected if name not in declared]
if drift or unknown:
    detail = drift + unknown
    suite_row('declared/runtime mutation mismatch: '
              + ', '.join(detail)
              + f'; declared={sorted(declared)} executed={sorted(runtime_names)}')
    publish_summary()
    print(f'switch denominator drifted: {detail}', file=sys.stderr)
    sys.exit(1)
if selected:
    mutations = [row for row in mutations if row[0] in selected]

try:
    with mutating_sources(root, sorted({source / row[1] for row in mutations}),
                          baseline=baseline) as snapshots:

        def run(full):
            result = run_owned(['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
                                '-p', 'pilotdeck-gateway', '--lib',
                                full, '--', '--exact'], cwd=root, env=env, timeout=600)
            return result.returncode, result.stdout + result.stderr

        # #819: an unmutated baseline for every bound test must be green first.
        baseline_ok = set()
        for test in dict.fromkeys(row[4] for row in mutations):
            wait_for_slot('baseline-' + test)
            code, log = run(test)
            (out / f'baseline-{test}.log').write_text(log)
            ok = code == 0 and baseline_passed(log, test)
            print(f'baseline {test}: {"PASS" if ok else "INVALID"}', flush=True)
            if ok:
                baseline_ok.add(test)
            else:
                failures.append('baseline-' + test)

        for name, filename, old, new, test, assertion in mutations:
            path = source / filename
            original = snapshots[path]
            before = hashlib.sha256(original).hexdigest()
            content = original.decode()
            status, reason, code = 'INVALID', 'not run', None
            try:
                assert content.count(old) == 1, (name, content.count(old))
                changed = content.replace(old, new, 1)
                (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
                    content.splitlines(True), changed.splitlines(True),
                    fromfile=filename, tofile=filename)))
                wait_for_slot(name)
                path.write_text(changed)
                code, log = run(test)
                (out / f'{name}.log').write_text(log)
                evidence = panic_evidence(log, test)
                # Kill evidence is the bound probe assertion (a callback's
                # try_lock probe re-acquiring the guard, the probe sequence
                # breaking, the bounded re-entrant call missing its deadline)
                # or the #1600 fixture assertion. The marker list still admits
                # the sequence/deadline anchors a panic may hit first.
                marker_ok = any(marker in evidence for marker in (
                    assertion,
                    'every probe fires exactly once, in order',
                    're-entrant bus call completed within the timeout'))
                killed = (test in baseline_ok and code == 101
                          and named_test_failed(log, test)
                          and marker_ok
                          and 'could not compile' not in log)
                reason = next((line.strip() for line in log.splitlines()
                               if 'panicked at' in line or 'assertion' in line), '')
                status = 'KILLED' if killed else (
                    'SURVIVED' if code == 0 else 'INVALID')
            except Exception as error:
                status = 'INVALID'
                reason = f'{type(error).__name__}: {error}'
            finally:
                path.write_bytes(original)
                if hashlib.sha256(path.read_bytes()).hexdigest() != before:
                    raise RuntimeError(f'byte restoration failed: {filename}')
            # Restored phase: the bytes put back must run the bound test green
            # again, so a mutant that left a side effect cannot pass as killed.
            try:
                wait_for_slot(name + '-restored')
                code_r, log_r = run(test)
                (out / f'{name}-restored.log').write_text(log_r)
                if not (code_r == 0 and baseline_passed(log_r, test)):
                    status = 'INVALID'
                    reason = (reason + '; restored control did not pass'
                              if reason else 'restored control did not pass')
            except Exception as error:
                status = 'INVALID'
                reason = ((reason + f'; restore test: {type(error).__name__}: {error}')
                          if reason else f'restore test: {type(error).__name__}: {error}')
            summary.append(dict(name=name, test=test, assertion=assertion,
                                status=status, returncode=code, reason=reason,
                                sourceSha256=before))
            say(f'{name}: {status} rc={code} {reason}')
            if status != 'KILLED':
                failures.append(name)
except BaseException as error:
    # Same contract as the sibling fixtures (_shared/negative_control.py,
    # user-dialog-guard-order): a guard failure publishes a complete table with
    # every row INVALID instead of the partial green rows. (SIGTERM / SIGKILL
    # bypass this and leave no summary at all — absence is a fail-closed read.)
    reason = f'source guard: {type(error).__name__}: {error}'
    recorded = {row['name'] for row in summary}
    for row in summary:
        row['status'] = 'INVALID'
        row['reason'] = f'{row["reason"]}; {reason}' if row.get('reason') else reason
    for name, _filename, _old, _new, test, assertion in mutations:
        if name not in recorded:
            summary.append(dict(
                name=name, test=test, assertion=assertion,
                status='INVALID', returncode=None, reason=reason,
                sourceSha256=None))
    publish_summary()
    raise

# Runtime truncation check: the loop must have produced exactly one row per
# selected mutation. An emptied table reaches here with no rows at all.
recorded = [row['name'] for row in summary]
missing = [row[0] for row in mutations if row[0] not in recorded]
if not summary or missing or len(summary) != len(mutations):
    suite_row('runtime execution table incomplete: '
              f'missing={missing} rows={len(summary)} expected={len(mutations)}'
              + ('' if summary else '; empty execution table'))
counts = publish_summary()
print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
if failures or any(row['status'] != 'KILLED' for row in summary):
    print(f'not killed: {", ".join(failures) or "see summary"}', file=sys.stderr)
    sys.exit(1)
print(f'{counts["KILLED"]}/{len(declared)} declared mutations killed; '
      'every source restored byte-for-byte', flush=True)

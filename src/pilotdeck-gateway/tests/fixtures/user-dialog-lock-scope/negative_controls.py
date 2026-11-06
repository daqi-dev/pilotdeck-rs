#!/usr/bin/env python3
"""Temporary product mutations; require an oracle assertion failure, then restore.

#1442/#1599/#1600 negative controls: each mutation puts one host-supplied
callback (the resolver, the settle hook, the accepts validator, or the change
observer) back under the GatewayUserDialogBus MutexGuard, or drifts the shared
`management_tests::setup` fixture back to the retired unconnected bus; the
matching probe test must turn red. Every mutant keeps the callback call count
and order of the original code, so the kill evidence is the "runs under the
guard" probe rather than a duplicated callback. Run with no other
writers/builds in this worktree. Evidence goes in a new output folder;
original source bytes and hashes are verified after every mutation.

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
     '                            (consumed.entry.resolve)(answer);'),
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
     '                            };'),
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
     '                        if !accepted {'),
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
     '    }'),
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
     '            )),'),
]
declared = [name for name, *_ in mutations]
selected = sys.argv[3:]
if selected:
    mutations = [mutation for mutation in mutations if mutation[0] in selected]
    assert len(mutations) == len(selected), 'unknown mutation name'
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR'):
    sys.exit('CARGO_TARGET_DIR must be supplied by the caller')
summary = []
failures = []


def target_test(name):
    # Full test path: the fixture mutation lives in management_tests, the
    # callback probes in methods::tests.
    if name == 'fixture-drifts-to-unconnected-bus':
        return ('native_process::management_tests::'
                'setup_assembles_a_connected_user_dialog_bus')
    if name == 'observer-delivery-back-under-guard':
        return 'native_process::methods::tests::user_dialog_observer_reenters_the_bus_lock_free'
    if name == 'accepts-back-under-guard':
        return 'native_process::methods::tests::user_dialog_accepts_reenters_the_bus_lock_free'
    return 'native_process::methods::tests::user_dialog_settle_and_resolve_reenter_the_bus_lock_free'


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
    # distinguishable in the evidence.
    counts = {state: sum(row['status'] == state for row in summary)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    payload = dict(total=len(declared), declared=declared,
                   selected=[row['name'] for row in summary],
                   counts=counts, results=summary)
    tmp = out / 'summary.json.tmp'
    tmp.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + '\n')
    os.replace(tmp, out / 'summary.json')
    return counts


try:
    with mutating_sources(root, sorted({source / filename for _, filename, _, _ in mutations}),
                          baseline=baseline) as snapshots:
        def run(full):
            result = run_owned(['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
                                '-p', 'pilotdeck-gateway', '--lib',
                                full, '--', '--exact'], cwd=root, env=env, timeout=600)
            return result.returncode, result.stdout + result.stderr

        # #819: an unmutated baseline for every named target must be green first.
        baseline_ok = set()
        for test in dict.fromkeys(target_test(name) for name, *_ in mutations):
            full = test
            wait_for_slot('baseline-' + test)
            code, log = run(full)
            (out / f'baseline-{test}.log').write_text(log)
            ok = code == 0 and baseline_passed(log, full)
            print(f'baseline {test}: {"PASS" if ok else "INVALID"}', flush=True)
            if ok:
                baseline_ok.add(test)
            else:
                failures.append('baseline-' + test)

        for name, filename, old, new in mutations:
            path = source / filename
            original = snapshots[path]
            before = hashlib.sha256(original).hexdigest()
            content = original.decode()
            test = target_test(name)
            full = test
            try:
                assert content.count(old) == 1, (name, content.count(old))
                changed = content.replace(old, new, 1)
                (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
                    content.splitlines(True), changed.splitlines(True),
                    fromfile=filename, tofile=filename)))
                wait_for_slot(name)
                path.write_text(changed)
                code, log = run(full)
                (out / f'{name}.log').write_text(log)
                evidence = panic_evidence(log, full)
                # Kill evidence is one of the #1442 probe assertions (a
                # callback's try_lock probe failing, the probe sequence
                # breaking, the bounded re-entrant call missing its deadline)
                # or the #1600 fixture assertion.
                marker_ok = any(marker in evidence for marker in (
                    'must not run under the guard',
                    'every probe fires exactly once, in order',
                    "re-entrant bus call completed within the timeout",
                    "the shared fixture's user-dialog bus must be connected"))
                killed = (test in baseline_ok and code == 101
                          and named_test_failed(log, full)
                          and marker_ok
                          and 'could not compile' not in log)
                failure = next((line.strip() for line in log.splitlines()
                                if 'panicked at' in line or 'assertion' in line), '')
                status = 'KILLED' if killed else (
                    'SURVIVED' if code == 0 else 'INVALID')
                summary.append(dict(name=name, test=full, status=status,
                                    returncode=code, reason=failure,
                                    sourceSha256=before))
                say(f'{name}: {status} rc={code} {failure}')
                if status != 'KILLED':
                    failures.append(name)
            except Exception as error:
                summary.append(dict(name=name, test=full, status='INVALID',
                                    returncode=None,
                                    reason=f'{type(error).__name__}: {error}',
                                    sourceSha256=before))
                failures.append(name)
            finally:
                path.write_bytes(original)
                assert hashlib.sha256(path.read_bytes()).hexdigest() == before
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
    for name, *_rest in mutations:
        if name not in recorded:
            summary.append(dict(
                name=name,
                test=target_test(name),
                status='INVALID', returncode=None, reason=reason,
                sourceSha256=None))
    publish_summary()
    raise
counts = publish_summary()
print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
if failures:
    print(f'not killed: {", ".join(failures)}', file=sys.stderr)
    sys.exit(1)
print(f'{len(summary)}/{len(mutations)} killed; every source restored byte-for-byte', flush=True)

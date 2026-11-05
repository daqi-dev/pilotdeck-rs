#!/usr/bin/env python3
"""#1395: run after commit; HEAD/source guard is mandatory, never bypassed.

CARGO_TARGET_DIR=... python3 negative_controls.py NEW_OUTPUT_DIR [name ...]
Every mutant must compile before its bound runtime assertion can kill it.
A mutation registered in pending-reanchor.txt is skipped, never executed
(#1048 protocol, same semantics as negative_suite.run_cases).
"""
import argparse
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import pending_reanchor as pending
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, passed, invalidate_completed, publish_summary

script = Path(__file__).resolve()
root = script.parents[5]
source = root / 'src/pilotdeck-gateway/src/user_dialog/http_store.rs'
tests = root / 'src/pilotdeck-gateway/src/user_dialog/http_store_park_tests.rs'
PREFIX = 'user_dialog::http_store::http_store_park_tests::'
RACE = PREFIX + 'issue_1395_synthetic_zero_room_shutdown_window'
WIRE = PREFIX + 'issue_1395_production_hyper_reads_have_capacity'
CLOSE = PREFIX + 'issue_1395_shutdown_with_pending_or_cancelled_body_is_bounded'
ANCHOR_COUNTS = {'shutdown-timeouts-disabled': 2}
cases = [
    ('A-missing-park-hook', source,
     '        http_store_park_tests::before_park(&this.timer);',
     '        if false { http_store_park_tests::before_park(&this.timer); }',
     RACE, '#1395 A target park hook must be reached exactly once'),
    ('B-missing-hook-action', tests,
     '            hook();', '            drop(hook);',
     RACE, '#1395 B shutdown task must complete inside target hook'),
    ('C-missing-shutdown-event', tests,
     '                shutdown_timer.begin_shutdown();',
     '                let _ = &shutdown_timer;',
     RACE, '#1395 C begin_shutdown must change target state'),
    ('wrong-target', tests,
     '.find(|probe| Arc::ptr_eq(&probe.target, &timer.shutdown_started))',
     '.find(|probe| Arc::ptr_eq(&probe.target, &timer.shutdown_started) || probe.target.load(Ordering::SeqCst) == timer.shutdown_started.load(Ordering::SeqCst))',
     RACE, '#1395 foreign park must not hit target'),
    ('hyper-caller-zero-capacity', source,
     '        let this = self.get_mut();\n'
     '        #[cfg(test)]\n'
     '        http_store_park_tests::on_read(&this.timer, buffer.remaining());',
     '        let this = self.get_mut();\n'
     '        let _ = buffer.remaining();\n'
     '        let mut empty = [];\n'
     '        let buffer = &mut ReadBuf::new(&mut empty);\n'
     '        #[cfg(test)]\n'
     '        http_store_park_tests::on_read(&this.timer, buffer.remaining());',
     WIRE, '#1395 hyper must always provide at least 8192 bytes of read capacity'),
    ('missing-pending-body-observation', tests,
     '        probe.pending_bodies.fetch_add(1, Ordering::SeqCst);',
     '        probe.pending_bodies.fetch_add(0, Ordering::SeqCst);',
     CLOSE, '#1395 target body must be Pending with no connection deadline before shutdown'),
    ('shutdown-timeouts-disabled', source,
     'limits.request_timeout', 'Duration::from_secs(3600)',
     CLOSE, '#1395 unfinished or cancelled body cannot hang shutdown'),
    ('park-deadline-not-polled', source,
     '        this.timer.store_waker(cx);\n'
     '        if deadline.is_some() && this.sleep.as_mut().poll(cx).is_ready() {',
     '        this.timer.store_waker(cx);\n'
     '        if false && deadline.is_some() && this.sleep.as_mut().poll(cx).is_ready() {',
     RACE, '#1395 only an armed deadline rescues the synthetic park'),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out', type=Path)
    parser.add_argument('mutations', nargs='*')
    args = parser.parse_args()
    env = os.environ.copy()
    if not env.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be provided by the caller')
    unknown = set(args.mutations) - {row[0] for row in cases}
    if unknown:
        parser.error(f'unknown mutations: {sorted(unknown)}')
    selected = [row for row in cases if not args.mutations or row[0] in args.mutations]
    args.out.mkdir(parents=True, exist_ok=False)
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--lib']
    results = []

    def run(label, full, compile_only=False):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=300)
        (args.out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
        tail = ['--no-run', full] if compile_only else [full, '--', '--exact', '--include-ignored']
        result = run_owned(command + tail, cwd=root, env=env, timeout=900)
        (args.out / f'{label}.log').write_text(result.stdout + result.stderr)
        return result

    def record(name, full, assertion, status, reason, compiled=False):
        results.append(dict(mutation=name, test=full, assertion=assertion,
                            status=status, reason=reason, compiled=compiled))
        print(f'{name}: {status}: {reason}', flush=True)

    try:
        registrations = pending.for_script(root, script)
        guarded = [source, tests]
        if registrations:
            # Guard the registry too, like negative_suite.run_cases: a mid-run
            # edit must invalidate every row instead of silently re-reading it.
            guarded.append(root / pending.FIXTURES / pending.REGISTRY)
        with mutating_sources(root, guarded) as snapshots:
            for name, path, old, new, full, assertion in selected:
                entry = pending.match(registrations, name)
                if entry is not None:
                    # Skip before anchor check, mutation, and cargo
                    # (negative_suite.py:148-151 semantics).
                    record(name, full, assertion, pending.SKIPPED, entry.reason())
                    continue
                status, reason, compiled = 'INVALID', 'not run', False
                try:
                    original = snapshots[path].decode()
                    expected = ANCHOR_COUNTS.get(name, 1)
                    count = original.count(old)
                    if count != expected:
                        raise ValueError(f'{name}: expected {expected} anchors, found {count}')
                    if not passed(run(name + '-positive', full), full):
                        reason = 'positive bound test did not pass'
                    else:
                        path.write_text(original.replace(old, new))
                        built = run(name + '-compile', full, compile_only=True)
                        compiled = built.returncode == 0
                        if compiled:
                            status, reason = classify(run(name + '-negative', full),
                                                      Case(name, path, old, new, full, assertion))
                        else:
                            reason = 'mutant failed cargo test --no-run; not a runtime kill'
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    path.write_bytes(snapshots[path])
                try:
                    if not passed(run(name + '-restored', full), full):
                        status, reason = 'INVALID', reason + '; restored bound test did not pass'
                except Exception as error:
                    status, reason = 'INVALID', reason + f'; restored: {error}'
                record(name, full, assertion, status, reason, compiled)
    except BaseException as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for name, _path, _old, _new, full, assertion in selected[len(results):]:
            record(name, full, assertion, 'INVALID', reason)
    publish_summary(args.out, results)
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    if sys.argv[1:] == ['--check-anchors']:
        registrations = pending.for_script(root, script)
        stale = []
        for name, path, old, _new, _test, _assertion in cases:
            if pending.match(registrations, name) is not None:
                # Same as negative_suite.matrix_main --check: a registered
                # pending mutation is reported and the check fails closed.
                stale.append((name, pending.SKIPPED))
                continue
            count = path.read_text().count(old)
            if count != ANCHOR_COUNTS.get(name, 1):
                stale.append((name, count))
        if stale:
            raise SystemExit(f'STALE ANCHORS: {stale}')
        print(f'{len(cases)} anchors: ok')
        raise SystemExit(0)
    raise SystemExit(main())

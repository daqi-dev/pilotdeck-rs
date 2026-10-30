#!/usr/bin/env python3
# anchor-selfcheck: unsupported CASES contains lazy complete mutated files, not before fragments
"""Bounded, serial #543 mutations. Restore implementation bytes after every run.
Uses the caller's CARGO_TARGET_DIR; no scratch builds or background probes.
Usage: python3 negative-controls.py [--compile-only] <evidence-dir> [mutation-id ...]
"""
import hashlib
import os
from pathlib import Path
import subprocess
import sys

# As in plugin-loader/negative-controls.py: replay metadata validation stays on,
# but skip the baseline git subprocess storm after every mutant recompilation.
MUTANT_ENV = dict(os.environ)
MUTANT_ENV.pop('PDRS_BASELINE', None)

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed
from negative_process import mutating_sources, run_owned, wait_for_slot, new_output_directory
from negative_suite import Case, classify, invalidate_completed, passed, publish_summary
from negative_selection import select

ROOT = Path(__file__).resolve().parents[5]
SOURCE = ROOT / 'src/pilotdeck-gateway/src/runtime/plugins.rs'
args = sys.argv[1:]
compile_only = '--compile-only' in args
args = [arg for arg in args if arg != '--compile-only']
if not args or not os.environ.get('CARGO_TARGET_DIR'):
    raise SystemExit('set CARGO_TARGET_DIR; usage: [--compile-only] NEW_OUTPUT [mutation ...]')
OUTPUT = new_output_directory(args[0])
MUTANT_ENV['CARGO_TERM_QUIET'] = 'false'
summary = []
selected = []
try:
    with mutating_sources(ROOT, [SOURCE]) as snapshots:
        BASE = snapshots[SOURCE]
        TEXT = BASE.decode()
        def replace(old, new):
            assert TEXT.count(old) == 1, f'unique mutation anchor required: {old!r}'
            return TEXT.replace(old, new)

        def serial_mutation():
            serial = replace('            for (plugin, completion) in ready {',
                             '            let serial = std::sync::Arc::new(TokioMutex::new(()));\n            for (plugin, completion) in ready {\n                let serial = std::sync::Arc::clone(&serial);')
            assert serial.count('                let mut future = Box::pin(async move {') == 1, 'serial future anchor'
            serial = serial.replace('                let mut future = Box::pin(async move {',
                                    '                let mut future = Box::pin(async move {\n                    let _serial = serial.lock().await;')
            return serial

        def stop_mutation():
            return replace('                started.push((plugin, completion, callback, first));',
                       '                let stop = matches!(&first, std::task::Poll::Ready(Err(_)));\n                started.push((plugin, completion, callback, first));\n                if stop { break; }')
        CASES = [
            ('restore-old-order', lambda: replace('    registry.disposal_waiters.extend(woken);',
                                       '    registry.disposal_waiters.splice(0..0, woken);'), 'runtime_dispose_retired_and_active_together', '"dispose-retired-and-active-together" calls'),
            ('serial-instead-of-parallel', serial_mutation, 'runtime_dispose_active_order_and_parallel', '"dispose-active-order-and-parallel" step 3'),
            ('reverse-active-order', lambda: replace('    let pending = plugins\n        .into_iter()', '    let pending = plugins\n        .into_iter().rev()'),
             'runtime_dispose_active_order_and_parallel', '"dispose-active-order-and-parallel" step 3'),
            ('reverse-retired-waiters', lambda: replace('        start_ready_disposals(&mut st.registry, state_mutex);',
                                             '        st.registry.disposal_waiters.reverse();\n        start_ready_disposals(&mut st.registry, state_mutex);'),
             'runtime_dispose_retired_generations', '"dispose-retired-generations" step 15'),
            ('old-waiters-before-new', lambda: replace('    registry.disposal_waiters.extend(woken);',
                                             '    registry.disposal_waiters.splice(0..0, woken);'),
             'runtime_dispose_reactivated_retiree', '"dispose-reactivated-retiree" calls'),
            ('skip-callback-await', lambda: replace('std::task::Poll::Pending => callback.await,',
                                           'std::task::Poll::Pending => { drop(callback); Ok(()) },'),
             'runtime_dispose_active_order_and_parallel', '"dispose-active-order-and-parallel" step 4'),
            ('ignore-leases', lambda: replace('if registry.is_plugin_referenced(&plugin) {',
                                     'if false && registry.is_plugin_referenced(&plugin) {'),
             'runtime_dispose_retired_generations', '"dispose-retired-generations" step 11'),
            ('stop-on-callback-error', stop_mutation, 'runtime_dispose_errors_aggregate', '"dispose-errors-aggregate" step 2'),
            ('reverse-error-causes', lambda: replace('.map(|pending| pending.receiver.clone())\n                                .collect();',
                                             '.map(|pending| pending.receiver.clone()).rev()\n                                .collect();'),
             'runtime_dispose_mixed_errors_and_pending_callback', '"dispose-mixed-errors-and-pending-callback" step 14'),
            ('duplicate-retired-instance', lambda: replace('if !self.retired_plugins.iter().any(|old| Arc::ptr_eq(old, p)) {',
                                                   'if true {'),
             'runtime_dispose_reactivated_retiree', '\"dispose-reactivated-retiree\" step 15'),
            ('omit-group-error', lambda: replace('                        errors.push(group);', '                        let _ = group;'),
             'runtime_dispose_errors_aggregate', '"dispose-errors-aggregate" step 2'),
        ]
        selected = select(CASES, args[1:], always=())
        command = ['cargo', 'test', '--offline', '--manifest-path', str(ROOT/'src/Cargo.toml'),
                   '-p', 'pilotdeck-gateway', '--test', 'plugin_runtime']

        def run(label, test, compile=False):
            gate = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=900)
            (OUTPUT / f'{label}.slots.log').write_text(gate.stdout + gate.stderr)
            command_args = ['--no-run'] if compile else [test, '--', '--exact', '--nocapture', '--test-threads=1']
            try:
                result = run_owned(command + command_args, cwd=ROOT, env=MUTANT_ENV, timeout=900)
            except subprocess.TimeoutExpired as error:
                (OUTPUT / f'{label}.log').write_text(
                    (error.stdout or '') + (error.stderr or '') + '\nTIMEOUT\n')
                raise
            (OUTPUT / f'{label}.log').write_text(result.stdout + result.stderr)
            return result

        for name, build_mutation, test, marker in selected:
            row = dict(mutation=name, test=test, assertion=marker, compiled=False,
                       status='INVALID', reason='')
            try:
                mutation = build_mutation()
                row['mutationSha256'] = hashlib.sha256(mutation.encode()).hexdigest()
                if mutation.encode() == BASE:
                    raise ValueError('mutation must change source bytes')
                if not compile_only and not passed(run(name + '-positive', test), test):
                    raise RuntimeError('bound positive test failed')
                SOURCE.write_text(mutation)
                if run(name + '-compile', test, compile=True).returncode:
                    raise RuntimeError('mutant compilation failed')
                row['compiled'] = True
                if compile_only:
                    row.update(status='COMPILED', reason='mutant no-run passed')
                else:
                    result = run(name + '-negative', test)
                    status, reason = classify(result, Case(name, SOURCE, TEXT, mutation, test, marker))
                    if status == 'KILLED' and not named_test_killed(
                            result.stdout + result.stderr, test, marker):
                        status, reason = 'INVALID', 'missing bound assertion in named panic'
                    row.update(status=status, reason=reason)
            except Exception as error:
                row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
            finally:
                SOURCE.write_bytes(BASE)
            if not compile_only:
                try:
                    if not passed(run(name + '-restored', test), test):
                        raise RuntimeError('restored bound test failed')
                except Exception as error:
                    row.update(status='INVALID', reason=row['reason'] + f'; restored: {error}')
            summary.append(row)
            print(f'{name}: {row["status"]}: {row["reason"]}', flush=True)
except BaseException as error:
    invalidate_completed(summary, f'source guard: {error}')
    recorded = {row['mutation'] for row in summary}
    for name, build_mutation, test, marker in selected:
        if name not in recorded:
            summary.append(dict(mutation=name, test=test, assertion=marker, compiled=False,
                                status='INVALID', reason=f'source guard: {error}'))
    publish_summary(OUTPUT, summary)
    raise
publish_summary(OUTPUT, summary)
raise SystemExit(int(any(row['status'] != ('COMPILED' if compile_only else 'KILLED') for row in summary)))

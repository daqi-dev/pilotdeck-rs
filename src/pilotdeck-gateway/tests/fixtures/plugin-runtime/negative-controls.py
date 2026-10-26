#!/usr/bin/env python3
"""Bounded, serial #543 mutations. Restore implementation bytes after every run.
Uses the launcher's one src/target; no scratch builds or background probes.
Usage: python3 negative-controls.py <evidence-dir> [mutation-id ...]
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

# As in plugin-loader/negative-controls.py: replay metadata validation stays on,
# but skip the baseline git subprocess storm after every mutant recompilation.
MUTANT_ENV = dict(os.environ)
MUTANT_ENV.pop('PDRS_BASELINE', None)

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed
from negative_process import mutating_sources, run_owned, wait_for_slot

ROOT = Path(__file__).resolve().parents[5]
SOURCE = ROOT / 'src/pilotdeck-gateway/src/runtime/plugins.rs'
OUTPUT = Path(sys.argv[1]).resolve()
OUTPUT.mkdir(parents=True, exist_ok=True)
target_dir = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'src/target')).resolve()
assert target_dir in {(ROOT / 'src/target').resolve(), (ROOT / '_build/cargo-target').resolve()}, \
    'use this tree src/target or _build/cargo-target'
with mutating_sources(ROOT, [SOURCE]) as snapshots:
    BASE = snapshots[SOURCE]
    TEXT = BASE.decode()
    # Fixed pre-#543 implementation: HEAD already contains the fix after commit or
    # merge, so it cannot be used as the source of this regression mutation.
    OLD_COMMIT = 'dbaa207c549eb42dd3dd01a78828085c8ee48f98'
    OLD = subprocess.check_output(['git', 'show', f'{OLD_COMMIT}:src/pilotdeck-gateway/src/runtime/plugins.rs'],
                                  cwd=ROOT, timeout=10).decode()

    def replace(old, new):
        assert TEXT.count(old) == 1, f'unique mutation anchor required: {old!r}'
        return TEXT.replace(old, new)

    serial = replace('            for (plugin, completion) in ready {',
                     '            let serial = std::sync::Arc::new(TokioMutex::new(()));\n            for (plugin, completion) in ready {\n                let serial = std::sync::Arc::clone(&serial);')
    assert serial.count('                let mut future = Box::pin(async move {') == 1, 'serial future anchor'
    serial = serial.replace('                let mut future = Box::pin(async move {',
                            '                let mut future = Box::pin(async move {\n                    let _serial = serial.lock().await;')
    stop = replace('                started.push((plugin, completion, callback, first));',
                   '                let stop = matches!(&first, std::task::Poll::Ready(Err(_)));\n                started.push((plugin, completion, callback, first));\n                if stop { break; }')
    CASES = [
        ('restore-old-order', OLD, 'runtime_dispose_retired_and_active_together', '"dispose-retired-and-active-together" calls'),
        ('serial-instead-of-parallel', serial, 'runtime_dispose_active_order_and_parallel', '"dispose-active-order-and-parallel" step 3'),
        ('reverse-active-order', replace('    let pending = plugins\n        .into_iter()', '    let pending = plugins\n        .into_iter().rev()'),
         'runtime_dispose_active_order_and_parallel', '"dispose-active-order-and-parallel" step 3'),
        ('reverse-retired-waiters', replace('        start_ready_disposals(&mut st.registry, state_mutex);',
                                         '        st.registry.disposal_waiters.reverse();\n        start_ready_disposals(&mut st.registry, state_mutex);'),
         'runtime_dispose_retired_generations', '"dispose-retired-generations" step 15'),
        ('old-waiters-before-new', replace('    registry.disposal_waiters.extend(woken);',
                                         '    registry.disposal_waiters.splice(0..0, woken);'),
         'runtime_dispose_reactivated_retiree', '"dispose-reactivated-retiree" calls'),
        ('skip-callback-await', replace('std::task::Poll::Pending => callback.await,',
                                       'std::task::Poll::Pending => { drop(callback); Ok(()) },'),
         'runtime_dispose_active_order_and_parallel', '"dispose-active-order-and-parallel" step 4'),
        ('ignore-leases', replace('if registry.is_plugin_referenced(&plugin) {',
                                 'if false && registry.is_plugin_referenced(&plugin) {'),
         'runtime_dispose_retired_generations', '"dispose-retired-generations" step 11'),
        ('stop-on-callback-error', stop, 'runtime_dispose_errors_aggregate', '"dispose-errors-aggregate" step 2'),
        ('reverse-error-causes', replace('.map(|pending| pending.receiver.clone())\n                                .collect();',
                                         '.map(|pending| pending.receiver.clone()).rev()\n                                .collect();'),
         'runtime_dispose_mixed_errors_and_pending_callback', '"dispose-mixed-errors-and-pending-callback" step 14'),
        ('duplicate-retired-instance', replace('if !self.retired_plugins.iter().any(|old| Arc::ptr_eq(old, p)) {',
                                               'if true {'),
         'runtime_dispose_reactivated_retiree', '\"dispose-reactivated-retiree\" step 15'),
        ('omit-group-error', replace('                        errors.push(group);', '                        let _ = group;'),
         'runtime_dispose_errors_aggregate', '"dispose-errors-aggregate" step 2'),
    ]
    # Validate every mutation before starting cargo or writing implementation bytes.
    # In particular, a stale historical source must fail here, not masquerade as a
    # surviving behavioral mutation and prevent the remaining controls from running.
    for name, mutation, _, _ in CASES:
        assert mutation.encode() != BASE, f'{name}: mutation must change source bytes'
    selected = set(sys.argv[2:])
    assert not selected - {case[0] for case in CASES}
    summary = []
    deadline = time.monotonic() + 590
    try:
        for name, mutation, test, marker in CASES:
            if selected and name not in selected:
                continue
            assert SOURCE.read_bytes() == BASE, 'source changed outside this mutation runner'
            wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=max(1, deadline-time.monotonic()))
            try:
                SOURCE.write_text(mutation)
                result = run_owned(['cargo', 'test', '--offline', '--manifest-path', str(ROOT/'src/Cargo.toml'),
                                    '-p', 'pilotdeck-gateway', '--test', 'plugin_runtime', test, '--', '--exact',
                                    '--nocapture', '--test-threads=1'], cwd=ROOT, env=MUTANT_ENV,
                                   timeout=max(1, min(120, deadline-time.monotonic())))
                log = result.stdout + result.stderr
                (OUTPUT / f'{name}.log').write_text(log)
                # #813: `test {test} ... FAILED` plus the marker inside that
                # test's panic block, not stdout or a sibling test.
                killed = (result.returncode == 101 and named_test_killed(log, test, marker)
                          and 'could not compile' not in log)
                summary.append({'mutation': name, 'mutationSha256': hashlib.sha256(mutation.encode()).hexdigest(),
                                'test': test, 'exit': result.returncode, 'killed': killed,
                                'marker': marker})
                print(json.dumps(summary[-1]), flush=True)
                assert killed, f'{name}: require selected behavioral assertion failure, see log'
            finally:
                SOURCE.write_bytes(BASE)
    finally:
        assert SOURCE.read_bytes() == BASE, 'restoration failed'
        (OUTPUT/'summary.json').write_text(json.dumps({'sourceSha256': hashlib.sha256(BASE).hexdigest(),
                                                     'restored': True, 'cases': summary}, indent=2)+'\n')

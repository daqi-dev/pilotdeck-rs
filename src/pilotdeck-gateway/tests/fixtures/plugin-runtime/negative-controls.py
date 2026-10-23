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

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import run_owned, wait_for_slot

ROOT = Path(__file__).resolve().parents[5]
SOURCE = ROOT / 'src/pilotdeck-gateway/src/runtime/plugins.rs'
OUTPUT = Path(sys.argv[1]).resolve()
OUTPUT.mkdir(parents=True, exist_ok=True)
assert Path(os.environ['CARGO_TARGET_DIR']).resolve() == ROOT / 'src/target'
BASE = SOURCE.read_bytes()
TEXT = BASE.decode()
OLD = subprocess.check_output(['git', 'show', 'HEAD:src/pilotdeck-gateway/src/runtime/plugins.rs'], cwd=ROOT).decode()

def replace(old, new):
    assert TEXT.count(old) == 1, f'unique mutation anchor required: {old!r}'
    return TEXT.replace(old, new)

serial = replace('    for plugin in std::mem::take(&mut registry.disposal_waiters) {',
                 '    let mut previous: Option<DisposalReceiver> = None;\n    for plugin in std::mem::take(&mut registry.disposal_waiters) {')
serial = serial.replace('        let tx = pending.sender.take().unwrap();',
                        '        let tx = pending.sender.take().unwrap();\n        let preceding = previous.replace(pending.receiver.clone());')
serial = serial.replace('        let mut callback = Box::pin(async move {',
                        '        let mut callback = Box::pin(async move {\n            if let Some(preceding) = preceding { let _ = wait_watch(preceding).await; }')
stop = replace('        let state = Arc::clone(state_mutex);\n        tokio::spawn(async move {',
               '        let stop = matches!(&first, std::task::Poll::Ready(Err(_)));\n        let state = Arc::clone(state_mutex);\n        tokio::spawn(async move {')
stop = stop.replace('                .retain(|p| !Arc::ptr_eq(p, &plugin));\n        });',
                    '                .retain(|p| !Arc::ptr_eq(p, &plugin));\n        });\n        if stop { break; }')
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
    ('stop-on-callback-error', stop, 'runtime_dispose_errors_aggregate', 'dispose-errors-aggregate timed out'),
    ('reverse-error-causes', replace('.map(|pending| pending.receiver.clone())\n                            .collect();',
                                     '.map(|pending| pending.receiver.clone()).rev()\n                            .collect();'),
     'runtime_dispose_mixed_errors_and_pending_callback', '"dispose-mixed-errors-and-pending-callback" step 14'),
    ('duplicate-retired-instance', replace('if !self.retired_plugins.iter().any(|old| Arc::ptr_eq(old, p)) {',
                                           'if true {'),
     'runtime_dispose_reactivated_retiree', '\"dispose-reactivated-retiree\" step 15'),
    ('omit-group-error', replace('                        errors.push(group);', '                        let _ = group;'),
     'runtime_dispose_errors_aggregate', '"dispose-errors-aggregate" step 2'),
]
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
                                '--nocapture', '--test-threads=1'], cwd=ROOT,
                               timeout=max(1, min(120, deadline-time.monotonic())))
            log = result.stdout + result.stderr
            (OUTPUT / f'{name}.log').write_text(log)
            killed = (result.returncode == 101 and f'test {test} ... FAILED' in log and marker in log
                      and 'could not compile' not in log)
            summary.append({'mutation': name, 'test': test, 'exit': result.returncode, 'killed': killed,
                            'marker': marker})
            print(json.dumps(summary[-1]), flush=True)
            assert killed, f'{name}: require selected behavioral assertion failure, see log'
        finally:
            SOURCE.write_bytes(BASE)
finally:
    assert SOURCE.read_bytes() == BASE, 'restoration failed'
    (OUTPUT/'summary.json').write_text(json.dumps({'sourceSha256': hashlib.sha256(BASE).hexdigest(),
                                                 'restored': True, 'cases': summary}, indent=2)+'\n')

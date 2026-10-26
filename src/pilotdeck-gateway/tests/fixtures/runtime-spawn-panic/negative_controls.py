#!/usr/bin/env python3
"""Run after the patch is committed; mutate only under the shared source guard."""
import json
import os
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
if not os.environ.get('CARGO_TARGET_DIR'):
    sys.exit('CARGO_TARGET_DIR must be supplied by the caller')
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
mcp = 'src/pilotdeck-gateway/src/runtime/mcp/registry.rs'
pub = 'src/pilotdeck-gateway/src/runtime/turn/publish.rs'
fixture = 'src/pilotdeck-gateway/tests/fixtures/runtime-spawn-panic/oracle.json'
shared = 'shared_spawn_panic_matrix'
durable = 'durable_spawn_panic_matrix'
payload = 'shared_spawn_outcomes_and_cancelled_gate'
# Every mutant has its own test and runtime assertion anchor. No compile failures count.
mutants = [
    ('shared-join', mcp, r'let outcome = task.await.unwrap_or_else\(\|error\| \{.*?\n        \}\);', 'let outcome = task.await.unwrap();', shared, 'shared failure:'),
    ('shared-retain', mcp, r'sender.send_replace\(Some\(outcome\)\);', 'drop((sender, outcome));', shared, 'shared failure:'),
    ('shared-owned-payload', mcp, re.escape('.or_else(|| payload.downcast_ref::<String>().map(String::as_str))'), '', payload, 'panic payload'),
    ('shared-fallback', mcp, re.escape('.unwrap_or("MCP lifecycle task ended unexpectedly")'), '.unwrap_or("wrong fallback")', payload, 'panic payload'),
    ('durable-join', pub, r'let result = publication\s*\.executor\s*\.spawn\(future\).*?\n                \}\);', 'let result = future.await;', durable, 'spawn completion deadline'),
    ('durable-pending', pub, re.escape('state.pending.remove(&write);'), 'let _ = write;', durable, 'spawn completion deadline'),
    ('durable-notify', pub, r'(?m)^            publication\.changed\.notify_waiters\(\);', '            publication.changed.notify_one();', durable, 'spawn completion deadline|every drain waiter notified'),
    ('durable-failure', pub, re.escape('Err(_) if !state.failure_published =>'), 'Err(_) if false && !state.failure_published =>', durable, 'spawn completion deadline'),
    ('durable-once', pub, re.escape('Err(_) if !state.failure_published =>'), 'Err(_) =>', durable, 'single failure:'),
    ('shared-matrix', fixture, None, 'shared-', shared, 'complete shared matrix'),
    ('durable-matrix', fixture, None, 'durable-', durable, 'complete durable matrix'),
    ('shared-duplicate', fixture, None, 'shared-', shared, 'assertion failed: ids.insert(id)'),
    ('durable-duplicate', fixture, None, 'durable-', durable, 'assertion failed: ids.insert(id)'),
]

def record(name, data):
    with (out / name).open('x') as file:
        file.write(data)

def run(test, name):
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    record(name + '.slots.log', slots.stdout + slots.stderr)
    result = run_owned(['cargo', 'test', '--manifest-path', 'src/Cargo.toml', '-p',
        'pilotdeck-gateway', '--lib', test], cwd=root, env=os.environ.copy(), timeout=900)
    record(name + '.log', result.stdout + result.stderr)
    return result, result.stdout + result.stderr

for name, relative, pattern, replacement, test, anchor in mutants:
    path = root / relative
    with mutating_sources(root, [path]) as original:
        content = original[path].decode()
        if pattern is None:
            oracle = json.loads(content)
            index = next(i for i, c in enumerate(oracle['cases']) if c['id'].startswith(replacement))
            if name.endswith('-duplicate'): oracle['cases'].append(oracle['cases'][index].copy())
            else: oracle['cases'].pop(index)
            changed = json.dumps(oracle) + '\n'
        else:
            changed, count = re.subn(pattern, replacement, content, flags=re.S)
            assert count == 1, (name, count)
        path.write_text(changed)
        result, log = run(test, name)
        assert result.returncode == 101 and f'{test} ... FAILED' in log and any(part in log for part in anchor.split('|')) and 'test result: FAILED' in log, (name, 'not an anchored runtime failure')
        print(f'{name}: KILLED ({test}: {anchor})', flush=True)
    result, log = run(test, name + '-restored')
    assert result.returncode == 0 and f'{test} ... ok' in log, (name, 'restored test failed')
print(f'{len(mutants)} mutants killed and restored', flush=True)

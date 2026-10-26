#!/usr/bin/env python3
"""After commit, run with caller-owned CARGO_TARGET_DIR and no same-tree writers."""
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
source = 'src/pilotdeck-gateway/src/runtime/background.rs'
fixture = 'src/pilotdeck-gateway/tests/fixtures/background-panic/oracle.json'
snapshot, subagent = 'snapshot_panic_matrix', 'subagent_panic_matrix'
mutants = [
    ('write-join', source, r'(tokio::spawn\(\s*async move \{ Self::persist_entry_static\(&store, &entry\).await \},?\s*\)\s*\.await);', r'\1.unwrap();', snapshot, 'background panic completion deadline'),
    ('flush-join', source, re.escape('tokio::spawn(async move { store.flush().await }).await;'), 'tokio::spawn(async move { store.flush().await }).await.unwrap();', snapshot, 'background panic completion deadline'),
    ('continue-writes', source, re.escape('for entry in &entries {\n                    let (store, entry)'), 'for entry in entries.iter().take(1) {\n                    let (store, entry)', snapshot, 'continue after failure'),
    ('retain-disposed', source, re.escape('self.inner.disposed.send_replace(true);'), 'let _ = self.inner.disposed.send(true);', snapshot, 'retained disposed notification'),
    ('wake-disposed', source, re.escape('self.inner.disposed.send_replace(true);'), 'self.inner.disposed.send_if_modified(|value| { *value = true; false });', snapshot, 'every observer notified'),
    ('subagent-prefix', source, r'Some\(std::panic::catch_unwind\(std::panic::AssertUnwindSafe\(\s*\|\| run\(entry.controller.clone\(\)\),\s*\)\)\)', 'Some(Ok::<_, ()>(run(entry.controller.clone())))', subagent, 'background panic completion deadline'),
    ('subagent-poll', source, re.escape('Ok(body) => tokio::spawn(body).await.ok(),'), 'Ok(body) => Some(tokio::spawn(body).await.unwrap()),', subagent, 'background panic completion deadline'),
    ('subagent-notify', source, re.escape('entry.done.send_replace(true);\n        });'), 'drop(entry.done.subscribe());\n        });', subagent, 'background panic completion deadline'),
    ('cancel-before', source, re.escape('if task.status == TaskStatus::Cancelled {\n                    None'), 'if false {\n                    None', subagent, 'assertion `left == right` failed'),
    ('cancel-error', source, re.escape('Err(message) => {\n                        if task.status != TaskStatus::Cancelled {'), 'Err(message) => {\n                        if true {', subagent, 'assertion `left == right` failed'),
]
for prefix, test in [('write-', snapshot), ('flush-', snapshot), ('subagent-', subagent)]:
    for action in ['delete', 'duplicate', 'unknown']:
        mutants.append((prefix + action, fixture, None, (prefix, action), test, {'delete': 'complete matrix', 'duplicate': 'duplicate matrix case', 'unknown': 'unknown matrix case'}[action]))
mutants.append(('provenance', fixture, None, ('write-', 'provenance'), snapshot, 'oracle baseline commit'))

def record(name, text):
    with (out / name).open('x') as file:
        file.write(text)

def run(test, name):
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    record(name + '.slots.log', slots.stdout + slots.stderr)
    result = run_owned(['cargo', 'test', '--manifest-path', 'src/Cargo.toml', '-p',
        'pilotdeck-gateway', '--lib', test], cwd=root, env=os.environ.copy(), timeout=900)
    log = result.stdout + result.stderr
    record(name + '.log', log)
    return result.returncode, log

for name, relative, pattern, replacement, test, anchor in mutants:
    path = root / relative
    with mutating_sources(root, [path]) as original:
        content = original[path].decode()
        if pattern is None:
            data = json.loads(content)
            index = next(i for i, case in enumerate(data['cases']) if case['id'].startswith(replacement[0]))
            if replacement[1] == 'delete': data['cases'].pop(index)
            elif replacement[1] == 'duplicate': data['cases'].append(data['cases'][index].copy())
            elif replacement[1] == 'unknown': data['cases'][index]['id'] = replacement[0] + 'unknown'
            else: data['sourceCommit'] = 'invalid'
            changed = json.dumps(data) + '\n'
        else:
            changed, count = re.subn(pattern, replacement, content, flags=re.S)
            assert count == 1, (name, count)
        path.write_text(changed)
        code, log = run(test, name)
        assert code == 101 and f'{test} ... FAILED' in log and 'test result: FAILED' in log and anchor in log, (name, 'not an anchored runtime failure')
        print(f'{name}: KILLED', flush=True)
    code, log = run(test, name + '-restored')
    assert code == 0 and f'{test} ... ok' in log, (name, 'restored test failed')
print(f'{len(mutants)} mutants killed and restored', flush=True)

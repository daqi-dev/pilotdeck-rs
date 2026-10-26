#!/usr/bin/env python3
"""After commit, run with caller-owned CARGO_TARGET_DIR and no same-tree writers."""
import argparse
import json
import os
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
source = 'src/pilotdeck-gateway/src/runtime/background.rs'
fixture = 'src/pilotdeck-gateway/tests/fixtures/background-panic/oracle.json'
snapshot = 'snapshot_write_a::zero_before_sync'
subagent = 'subagent_sync'
mutants = [
    ('write-join', source, r'(tokio::spawn\(\s*async move \{ Self::persist_entry_static\(&store, &entry\).await \},?\s*\)\s*\.await);', r'\1.unwrap();', snapshot, 'background panic completion deadline'),
    ('flush-join', source, re.escape('tokio::spawn(async move { store.flush().await }).await;'), 'tokio::spawn(async move { store.flush().await }).await.unwrap();', 'snapshot_flush_pair::zero_before_sync', 'background panic completion deadline'),
    ('continue-writes', source, re.escape('for entry in &entries {\n                    let (store, entry)'), 'for entry in entries.iter().take(1) {\n                    let (store, entry)', snapshot, 'continue after failure'),
    ('retain-disposed', source, re.escape('self.inner.disposed.send_replace(true);'), 'let _ = self.inner.disposed.send(true);', snapshot, 'retained disposed notification'),
    ('wake-disposed', source, re.escape('self.inner.disposed.send_replace(true);'), 'self.inner.disposed.send_if_modified(|value| { *value = true; false });', 'snapshot_write_a::three_before_poll', 'every observer notified'),
    ('subagent-prefix', source, r'Some\(std::panic::catch_unwind\(std::panic::AssertUnwindSafe\(\s*\|\| run\(entry.controller.clone\(\)\),\s*\)\)\)', 'Some(Ok::<_, Box<dyn std::any::Any + Send>>(run(entry.controller.clone())))', subagent, 'background panic completion deadline'),
    ('subagent-poll', source, re.escape('Err(subagent_panic_message(error.try_into_panic().ok()))'), 'panic!("uncaught subagent poll")', 'subagent_poll', 'background panic completion deadline'),
    ('subagent-notify', source, re.escape('entry.done.send_replace(true);\n        });'), 'drop(entry.done.subscribe());\n        });', subagent, 'background panic completion deadline'),
    ('cancel-before', source, re.escape('if task.status == TaskStatus::Cancelled {\n                    None'), 'if false {\n                    None', 'subagent_cancel_before', 'assertion `left == right` failed'),
    ('cancel-error', source, re.escape('Err(message) => {\n                        if task.status != TaskStatus::Cancelled {'), 'Err(message) => {\n                        if true {', 'subagent_cancel_running', 'assertion `left == right` failed'),
]
# New edge controls each execute exactly one Rust matrix cell.
mutants.extend([
    ('empty-flush-join', source, re.escape('tokio::spawn(async move { store.flush().await }).await;'), 'tokio::spawn(async move { store.flush().await }).await.unwrap();', 'snapshot_flush_empty::three_before_poll', 'background panic completion deadline'),
    ('write-tail-join', source, mutants[0][2], mutants[0][3], 'snapshot_write_b::three_before_poll', 'background panic completion deadline'),
    ('sync-message', source, re.escape('Err(payload) => Err(subagent_panic_message(Some(payload))),'), 'Err(_) => Err(JsString::from("Background subagent task panicked.")),', 'subagent_sync', 'assertion `left == right` failed'),
    ('poll-message', source, re.escape('Err(subagent_panic_message(error.try_into_panic().ok()))'), 'Err(JsString::from("Background subagent task panicked."))', 'subagent_poll', 'assertion `left == right` failed'),
    ('fallback-message', source, re.escape('.unwrap_or("Background subagent task panicked.")'), '.unwrap_or("wrong fallback")', 'subagent_non_string_sync', 'assertion `left == right` failed'),
])
for prefix, test in [('write-a-', snapshot), ('write-b-', 'snapshot_write_b::zero_before_poll'),
                     ('flush-empty-', 'snapshot_flush_empty::zero_before_poll'),
                     ('flush-pair-', 'snapshot_flush_pair::zero_before_poll'), ('subagent-', subagent)]:
    for action in ['delete', 'duplicate', 'unknown']:
        mutants.append((prefix + action, fixture, None, (prefix, action), test, {'delete': 'complete matrix', 'duplicate': 'duplicate matrix case', 'unknown': 'unknown matrix case or family'}[action]))
mutants.extend([
    ('foreign-family', fixture, None, ('write-a-', 'foreign'), snapshot, 'unknown matrix case or family'),
    ('empty-calls', fixture, None, ('flush-empty-0-before-poll', 'calls'), 'snapshot_flush_empty::zero_before_poll', 'continue after failure'),
    ('tail-calls', fixture, None, ('write-b-0-before-poll', 'calls'), 'snapshot_write_b::zero_before_poll', 'continue after failure'),
    ('provenance', fixture, None, ('write-a-', 'provenance'), snapshot, 'oracle baseline commit'),
])


def mutate(name, content, pattern, replacement):
    if pattern is None:
        data = json.loads(content)
        matches = [i for i, case in enumerate(data['cases']) if case['id'].startswith(replacement[0])]
        if not matches:
            raise RuntimeError(f'{name}: stale fixture anchor {replacement[0]}')
        index, action = matches[0], replacement[1]
        if action == 'delete': data['cases'].pop(index)
        elif action == 'duplicate': data['cases'].append(data['cases'][index].copy())
        elif action == 'unknown': data['cases'][index]['id'] = replacement[0] + 'unknown'
        elif action == 'foreign': data['cases'].append(dict(data['cases'][index], id='foreign-family-0-before-sync'))
        elif action == 'calls': data['cases'][index]['calls'].append('write:unobserved')
        elif action == 'provenance': data['sourceCommit'] = 'invalid'
        else: raise RuntimeError(f'{name}: unknown mutation action {action}')
        changed = json.dumps(data) + '\n'
    else:
        changed, count = re.subn(pattern, replacement, content, flags=re.S)
        if count != 1:
            raise RuntimeError(f'{name}: stale source anchor (expected 1, got {count})')
    if changed == content:
        raise RuntimeError(f'{name}: mutation made no change')
    return changed

def main():
    if not os.environ.get('CARGO_TARGET_DIR'):
        sys.exit('CARGO_TARGET_DIR must be supplied by the caller')
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out', type=Path)
    parser.add_argument('--only', action='append', default=[], help='mutant name; repeat to select more')
    parser.add_argument('--check-anchors', action='store_true', help='validate mutations without writing sources or running cargo')
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    if args.only:
        known = {mutant[0] for mutant in mutants}
        if set(args.only) - known or len(args.only) != len(set(args.only)):
            parser.error('unknown or duplicate --only mutant')
        selected = [mutant for mutant in mutants if mutant[0] in args.only]

    else:
        selected = mutants

    # Fail on stale anchors before any mutation; never classify an invalid control as SURVIVED.
    for name, relative, pattern, replacement, _test, _anchor in selected:
        mutate(name, (root / relative).read_text(), pattern, replacement)
    if args.check_anchors:
        print(f'{len(selected)} mutation anchors validated; no sources changed')
        sys.exit(0)

    def record(name, text):
        with (out / name).open('x') as file:
            file.write(text)

    def run(test, name):
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
        record(name + '.slots.log', slots.stdout + slots.stderr)
        result = run_owned(['cargo', 'test', '--manifest-path', 'src/Cargo.toml', '-p',
            'pilotdeck-gateway', '--lib', 'runtime::background::panic_tests::' + test, '--', '--exact'], cwd=root, env=os.environ.copy(), timeout=900)
        log = result.stdout + result.stderr
        record(name + '.log', log)
        return result.returncode, log

    for name, relative, pattern, replacement, test, anchor in selected:
        path = root / relative
        with mutating_sources(root, [path]) as original:
            content = original[path].decode()
            changed = mutate(name, content, pattern, replacement)
            path.write_text(changed)
            code, log = run(test, name)
            assert code == 101 and f'{test} ... FAILED' in log and 'test result: FAILED' in log and anchor in log, (name, 'not an anchored runtime failure')
            print(f'{name}: KILLED', flush=True)
        code, log = run(test, name + '-restored')
        assert code == 0 and f'{test} ... ok' in log, (name, 'restored test failed')
    print(f'{len(selected)} mutants killed and restored', flush=True)


if __name__ == "__main__":
    main()

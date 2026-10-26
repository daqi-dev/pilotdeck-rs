#!/usr/bin/env python3
"""Owned, bounded source mutations. Product files must match HEAD on entry.

Each selected original test passes, the mutated test must fail its bound behavior
assertion or explicit impossible-branch guard (not compilation), then the same test passes after guarded restore.
No other writers/cargo are allowed in this worktree during this script.
"""
import difflib
import hashlib
import json
import os
import re
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
network = root / 'src/pilotdeck-gateway/src/runtime/network.rs'
web = root / 'src/pilotdeck-gateway/src/runtime/tools/builtin/web/network.rs'
cases = [
    ('io-as-microtask', network,
     "            timer: None,\n            microtask: false,",
     "            timer: None,\n            microtask: true,",
     ['background_io_headers_fulfilled_tie_parent_last']),
    ('io-parent-abort', network,
     '        if aborted && parent_at.is_none() {\n            return Poll::Ready(Exchange::Aborted);',
     '        if aborted && parent_at.is_none() {\n            return Poll::Ready(Exchange::Timeout { before_abort: false });',
     [f'fetch_redirect_io_headers_{event}_{order}' for event in ['fulfilled', 'rejected', 'idle'] for order in ['parent_first', 'tie_parent_first', 'tie_parent_last']]),
    ('io-parent-abort-background', network,
     '        if aborted && parent_at.is_none() {\n            return Poll::Ready(Exchange::Aborted);',
     '        if aborted && parent_at.is_none() {\n            return Poll::Ready(Exchange::Timeout { before_abort: false });',
     [f'background_io_headers_{event}_{order}' for event in ['fulfilled', 'rejected', 'idle'] for order in ['parent_first', 'tie_parent_first', 'tie_parent_last']]),
    ('parent-first', network, 'let response_before_parent = parent_at.is_none_or(|at| {',
     'let response_before_parent = true || parent_at.is_none_or(|at| {',
     ['fetch_redirect_headers_rejected_parent_first']),
    ('tie-parent-first', network, '(!parent_first && event_at == parent_at)',
     '(parent_first && event_at == parent_at)',
     ['fetch_redirect_headers_rejected_tie_parent_first']),
    ('tie-parent-last', network, 'event_at < parent_at || (!parent_first && event_at == parent_at)',
     'event_at < parent_at',
     ['fetch_redirect_headers_rejected_tie_parent_last_overdue', 'web_fetch_headers_fulfilled_tie_parent_last_overdue']),
    ('retry-tie', network, 'timer_precedes_parent(next, at, false)',
     'timer_precedes_parent(next, at, true)',
     [f'{path}_{phase}_fulfilled_tie_parent_last_overdue' for path in ['web_fetch', 'fetch_redirect'] for phase in ['retry_status', 'retry_error']]),
    ('retry-message', web, 'Exchange::Aborted => Err(abort_error("Network retry aborted.")),',
     'Exchange::Aborted => Err(abort_error("wrong retry message")),',
     ['fetch_redirect_retry_error_fulfilled_parent_first', 'fetch_redirect_retry_status_fulfilled_parent_first']),
    ('retry-outer-parent-message', web, 'if outer.parent {\n                abort_error("Network retry aborted.")',
     'if outer.parent {\n                abort_error("wrong outer retry message")',
     ['web_tools::web_fetch_hop_timeout_during_retry_uses_retry_abort_message']),
    ('retry-count', web, '&& attempt < retry.max_retries',
     '&& false && attempt < retry.max_retries',
     ['fetch_redirect_retry_status_fulfilled_continuation_first']),
    ('body-content', web, 'out.extend_from_slice(&data);',
     'out.extend_from_slice(&data[..1]);',
     ['web_fetch_body_fulfilled_continuation_first']),
    ('body-parent-name-delivery', network, 'if aborted {\n            return Poll::Ready(Exchange::Aborted);\n        }\n        if !response_first',
     'if false && aborted {\n            return Poll::Ready(Exchange::Aborted);\n        }\n        if !response_first',
     ['fetch_redirect_body_fulfilled_tie_parent_first', 'fetch_redirect_text_body_fulfilled_tie_parent_first']),
    ('matrix-parent-abort', network,
     'if aborted {\n            return Poll::Ready(Exchange::Aborted);\n        }\n        if !response_first',
     'if aborted {\n            return Poll::Ready(Exchange::Timeout { before_abort: false });\n        }\n        if !response_first', ['*']),
    ('matrix-response', network,
     'return Poll::Ready(Exchange::Response(result, aborted));',
     'let _ = result; return Poll::Ready(Exchange::Aborted);', ['*']),
    ('matrix-frame', network,
     'return Poll::Ready(Exchange::Response(result, false));',
     'let _ = result; return Poll::Ready(Exchange::Aborted);', ['*']),
    ('matrix-timeout', network,
     'return Poll::Ready(Exchange::Timeout {\n                before_abort: aborted && timeout_before_parent,\n            });',
     'return Poll::Ready(Exchange::Aborted);', ['*']),
]
background = root / 'src/pilotdeck-gateway/src/runtime/background.rs'
cases += [
    ('background-stop-signal', background,
     '        entry.controller.cancel_synchronously();',
     '        // negative control: stop did not signal the running child',
     ['background_headers_fulfilled_parent_first', 'background_headers_fulfilled_tie_parent_first',
      'background_retry_status_fulfilled_parent_first', 'background_retry_error_fulfilled_tie_parent_first',
      'background_retry_error_fulfilled_parent_first'] +
     [f'background_headers_{event}_{order}' for event in ['rejected', 'idle'] for order in ['parent_first', 'tie_parent_first']] +
     [f'background_redirect_hop_{event}_{order}' for event in ['fulfilled', 'rejected', 'idle'] for order in ['parent_first', 'tie_parent_first']]),
    ('background-stop-sync', background,
     '        entry.controller.cancel_synchronously();',
     '        entry.controller.cancel_at(tokio::time::Instant::now());',
     ['background_retry_status_fulfilled_tie_parent_first', 'background_retry_error_fulfilled_tie_parent_first']),
    ('background-pending', background,
     '                if task.status == TaskStatus::Cancelled {\n                    None',
     '                if false && task.status == TaskStatus::Cancelled {\n                    None',
     ['background_lifecycle_pending_stop_ordered']),
    ('background-wait', background,
     '        let mut done = entry.done.subscribe();\n        let _ = done.wait_for(|done| *done).await;',
     '        let mut done = entry.done.subscribe();\n        let _ = &mut done; // negative control: skipped child completion',
     ['background_lifecycle_running_uncooperative_ordered']),
    ('background-done', background,
     '            entry.done.send_replace(true);',
     '            entry.done.send_replace(false);',
     ['background_lifecycle_pending_stop_ordered']),
    ('background-ended', background,
     '        task.ended_at = Some((self.now)());',
     '        // negative control: stop omitted ended_at',
     ['background_lifecycle_running_uncooperative_ordered']),
    ('background-ended-stable', background,
     '                if task.ended_at.is_none() {',
     '                if true {',
     ['background_lifecycle_running_uncooperative_ordered']),
    ('background-completed', background,
     '                            task.status = TaskStatus::Completed;',
     '                            task.status = TaskStatus::Running;',
     ['background_lifecycle_settled_fulfilled_ordered']),
    ('background-error', background,
     '                            task.error = Some(message);',
     '                            task.error = Some("wrong child failure".into());',
     ['background_lifecycle_settled_rejected_ordered']),
]
assertion_anchors = {
    'io-parent-abort': 'no fixture timeout',
    'background-pending': 'pending stop must not start child',
    'background-wait': 'stop_and_wait must remain pending until child settles',
    'background-completed': 'registry did not settle after child completion',
    'background-done': 'background done notification deadline',
}
# Full-suite controls still bind a concrete test and its exact oracle anchor;
# an unrelated red test must never satisfy their negative result.
required_failures = {
    'matrix-parent-abort': 'fetch_redirect/headers/fulfilled/parent-first',
    'matrix-response': 'background/headers/fulfilled/continuation-first',
    'matrix-frame': 'background/body/fulfilled/continuation-first',
    'matrix-timeout': 'background/headers/idle/continuation-first',
}

def assert_required_failure(name, log):
    if name in required_failures:
        cell = required_failures[name]
        test = cell.replace('/', '_').replace('-', '_')
        pattern = (r"thread '" + re.escape(test) + r"'[^\n]*\n" +
                   re.escape('assertion `left == right` failed: network timing cell: ' + cell) + r'\n')
        assert re.search(pattern, log), (name, 'bound test/anchor must fail', test, cell)

if sys.argv[2:]:
    selected = set(sys.argv[2:])
    cases = [case for case in cases if case[0] in selected]
    assert len(cases) == len(selected), 'unknown control'
env = os.environ.copy()
env['CARGO_TARGET_DIR'] = str(root / '_build/cargo-target')

def save(name, text):
    with (out / name).open('x') as stream:
        stream.write(text)

test_source = (root / 'src/pilotdeck-gateway/tests/network_timing.rs').read_text()
expected_passes = len(re.findall(r'^#\[(?:tokio::)?test(?:\([^\n]*\))?\]', test_source, re.M)) - len(re.findall(r'^#\[ignore(?: = [^\n]*)?\]', test_source, re.M))

def run(name, test):
    slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
    save(name + '.slots.log', slot.stdout + slot.stderr)
    target, _, selection = test.partition('::')
    target, selection = (target, selection) if selection else ('network_timing', test)
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway', '--test', target]
    command += ['--', '--nocapture', '--test-threads=1'] if test == '*' else [selection, '--', '--exact', '--nocapture', '--test-threads=1']
    result = run_owned(command, cwd=root, env=env, timeout=300)
    log = result.stdout + result.stderr
    save(name + '.log', 'command: ' + repr(command) + '\n' + log + f'\nexit: {result.returncode}\n')
    return result.returncode, log

summary = []
for name, path, old, new, tests in cases:
    for index, test in enumerate(tests):
        code, log = run(f'{name}-{index}-positive', test)
        assert code == 0 and (f'{expected_passes} passed; 0 failed' if test == '*' else '1 passed; 0 failed') in log, (name, 'positive', code)
    killed_cells = set()
    with mutating_sources(root, [path]) as originals:
        original = originals[path].decode()
        assert original.count(old) == 1, (name, 'unique mutation anchor', original.count(old))
        changed = original.replace(old, new)
        save(name + '.patch', ''.join(difflib.unified_diff(original.splitlines(True), changed.splitlines(True), fromfile=str(path.relative_to(root)), tofile=str(path.relative_to(root)))))
        path.write_text(changed)
        for index, test in enumerate(tests):
            code, log = run(f'{name}-{index}-negative', test)
            assert code == 101 and ('test result: FAILED.' in log if test == '*' else f"{test.split('::')[-1]} ..." in log and 'test result: FAILED. 0 passed; 1 failed' in log) and (f'left: Other({list(map(ord, "wrong outer retry message"))})' in log and f'right: Other({list(map(ord, "Network retry aborted."))})' in log and 'assertion `left == right` failed' in log if test.startswith('web_tools::') else assertion_anchors.get(name, 'assertion `left == right` failed: network timing cell:') in log), (name, 'not an oracle assertion failure', code)
            assert_required_failure(name, log)
            for line in log.splitlines():
                if '{"id":' in line:
                    row = json.loads(line[line.index('{"id":'):])
                    if row['ts'] != row['rust']:
                        killed_cells.add(row['id'])

    for index, test in enumerate(tests):
        code, log = run(f'{name}-{index}-restored', test)
        assert code == 0 and (f'{expected_passes} passed; 0 failed' if test == '*' else '1 passed; 0 failed') in log, (name, 'restored', code)
    summary.append({'name': name, 'tests': tests, 'requiredFailure': required_failures.get(name), 'assertionAnchor': assertion_anchors.get(name, 'assertion `left == right` failed: network timing cell:'), 'killed': True, 'restoredPass': True, 'sourceSha256': hashlib.sha256(original.encode()).hexdigest(), 'killedCells': sorted(killed_cells)})
    print(f'{name}: {len(tests)} test selections rejected, restored PASS', flush=True)
save('summary.json', json.dumps(summary, indent=2) + '\n')

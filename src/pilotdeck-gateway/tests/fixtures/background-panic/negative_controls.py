#!/usr/bin/env python3
"""After commit, run with caller-owned CARGO_TARGET_DIR and no same-tree writers."""
import argparse
import json
import os
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed
from negative_selection import baseline_passed
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
source = 'src/pilotdeck-gateway/src/runtime/background.rs'
fixture = 'src/pilotdeck-gateway/tests/fixtures/background-panic/oracle.json'
snapshot = 'snapshot_write_a::zero_before_sync'
subagent = 'subagent_sync'
# #1233: dispose's final flush is joined into a memoized outcome; the mutant
# swallows the rejection so dispose reports fulfilled on a failed flush.
flush_outcome = (
    '                outcome = match tokio::spawn(async move { store.flush().await }).await {\n'
    '                    Ok(Ok(())) => Ok(()),\n'
    '                    Ok(Err(error)) => Err(Arc::new(BackgroundTaskError::error(JsString::from(\n'
    '                        error.to_string().as_str(),\n'
    '                    )))),\n'
    '                    Err(joined) => {\n'
    '                        let payload = joined.into_panic();\n'
    '                        let message = crate::runtime::cleanup_task::panic_message(payload.as_ref())\n'
    '                            .map(str::to_owned)\n'
    '                            .unwrap_or_else(|| "Background snapshot flush failed.".to_owned());\n'
    '                        Err(Arc::new(BackgroundTaskError::error(JsString::from(\n'
    '                            message.as_str(),\n'
    '                        ))))\n'
    '                    }\n'
    '                };'
)
flush_swallow = '                let _ = tokio::spawn(async move { store.flush().await }).await;'
mutants = [
    ('write-join', source, r'(tokio::spawn\(\s*async move \{ Self::persist_entry_static\(&store, &entry\).await \},?\s*\)\s*\.await);', r'\1.unwrap();', snapshot, 'background panic completion deadline'),
    ('flush-join', source, re.escape(flush_outcome), flush_swallow, 'snapshot_flush_pair::zero_before_sync', 'disposal outcome'),
    ('continue-writes', source, re.escape('for entry in &entries {\n                    let (store, entry)'), 'for entry in entries.iter().take(1) {\n                    let (store, entry)', snapshot, 'continue after failure'),
    # With no live receiver, send() drops the outcome instead of storing it,
    # so every later dispose() waits on a watch that never settles.
    ('retain-disposed', source, re.escape('self.inner.disposed.send_replace(Some(outcome));'), 'let _ = self.inner.disposed.send(Some(outcome));', snapshot, 'background panic completion deadline'),
    # The closure still stores the outcome but returns false, so no waiter is
    # woken: the value is already set when the observers poll again, yet the
    # recorded wakeups stay clear.
    ('wake-disposed', source, re.escape('self.inner.disposed.send_replace(Some(outcome));'), 'self.inner.disposed.send_if_modified(|value| { *value = Some(outcome); false });', 'snapshot_write_a::three_before_poll', 'every observer notified'),
    ('subagent-prefix', source, r'Some\(std::panic::catch_unwind\(std::panic::AssertUnwindSafe\(\s*\|\| run\(entry.controller.clone\(\)\),\s*\)\)\)', 'Some(Ok::<_, Box<dyn std::any::Any + Send>>(run(entry.controller.clone())))', subagent, 'background panic completion deadline'),
    ('subagent-poll', source, re.escape('Err(subagent_panic_message(error.try_into_panic().ok()))'), 'panic!("uncaught subagent poll")', 'subagent_poll', 'background panic completion deadline'),
    ('subagent-notify', source, re.escape('entry.done.send_replace(true);\n            drop(clock);\n        });'), 'drop(entry.done.subscribe());\n            drop(clock);\n        });', subagent, 'background panic completion deadline'),
    ('cancel-before', source, re.escape('if task.status == TaskStatus::Cancelled {\n                    None'), 'if false {\n                    None', 'subagent_cancel_before', '#818 subagent'),
    ('cancel-error', source, re.escape('Err(message) => {\n                        if task.status != TaskStatus::Cancelled {'), 'Err(message) => {\n                        if true {', 'subagent_cancel_running', '#818 subagent'),
]
# New edge controls each execute exactly one Rust matrix cell.
mutants.extend([
    ('empty-flush-join', source, re.escape(flush_outcome), flush_swallow, 'snapshot_flush_empty::three_before_poll', 'disposal outcome'),
    ('write-tail-join', source, mutants[0][2], mutants[0][3], 'snapshot_write_b::three_before_poll', 'background panic completion deadline'),
    ('sync-message', source, re.escape('Err(payload) => Err(subagent_panic_message(Some(payload))),'), 'Err(_) => Err(JsString::from("Background subagent task panicked.")),', 'subagent_sync', '#818 subagent'),
    ('poll-message', source, re.escape('Err(subagent_panic_message(error.try_into_panic().ok()))'), 'Err(JsString::from("Background subagent task panicked."))', 'subagent_poll', '#818 subagent'),
    # The fallback literal only matters for a non-string payload. Bind the
    # mutant to that test's own message, not the generic `#818 subagent`
    # prefix used by the string-payload cells (host r1: the unmarked
    # `non_string_panic` assertion failed, so the marker never appeared).
    ('fallback-message', source, re.escape('.unwrap_or("Background subagent task panicked.")'), '.unwrap_or("wrong fallback")', 'subagent_non_string_sync', '#818 subagent non-string error for sync'),
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

def judge(code, log, test, anchor):
    """#818/#930: the named test failed at its own panic-block marker."""
    full = 'runtime::background::panic_tests::' + test
    return (code == 101 and named_test_killed(log, full, anchor)
            and 'could not compile' not in log)


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
        full = 'runtime::background::panic_tests::' + test
        result = run_owned(['cargo', 'test', '--manifest-path', 'src/Cargo.toml', '-p',
            'pilotdeck-gateway', '--lib', full, '--', '--exact'], cwd=root,
            env=os.environ.copy(), timeout=900)
        log = result.stdout + result.stderr
        record(name + '.log', log)
        return result.returncode, log

    summary = []
    failures = []
    with mutating_sources(root, sorted({root / m[1] for m in selected})) as originals:
        for test in dict.fromkeys(m[4] for m in selected):
            code, log = run(test, 'baseline-' + test)
            ok = code == 0 and baseline_passed(
                log, 'runtime::background::panic_tests::' + test)
            summary.append({'mutation': 'baseline', 'test': test,
                            'status': 'PASS' if ok else 'INVALID'})
            print(f'baseline {test}: {"PASS" if ok else "INVALID"}', flush=True)
            if not ok:
                failures.append('baseline-' + test)

        for name, relative, pattern, replacement, test, anchor in selected:
            path = root / relative
            try:
                changed = mutate(name, originals[path].decode(), pattern, replacement)
                path.write_text(changed)
                code, log = run(test, name)
                killed = judge(code, log, test, anchor)
                reason = None if killed else (
                    'bound test still passed' if code == 0
                    else f'exit={code}: named assertion missing from its panic block')
            except Exception as error:
                killed, reason = False, f'{type(error).__name__}: {error}'
            finally:
                path.write_bytes(originals[path])
            status = 'KILLED' if killed else (
                'SURVIVED' if reason == 'bound test still passed' else 'INVALID')
            summary.append({'mutation': name, 'test': test, 'status': status,
                            'reason': reason})
            print(f'{name}: {status}' + (f' ({reason})' if reason else ''), flush=True)
            if status != 'KILLED':
                failures.append(name)
            with mutating_sources(root, [path]):
                code, log = run(test, name + '-restored')
            if not (code == 0 and baseline_passed(
                    log, 'runtime::background::panic_tests::' + test)):
                summary[-1].update(status='INVALID', reason='restored positive failed')
                if name not in failures:
                    failures.append(name)
            (out / 'summary.json').write_text(json.dumps(
                {'results': summary,
                 'counts': {s: sum(r['status'] == s for r in summary)
                            for s in ('KILLED', 'SURVIVED', 'INVALID')}},
                indent=2) + '\n')
    counts = {s: sum(r['status'] == s for r in summary)
              for s in ('KILLED', 'SURVIVED', 'INVALID')}
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()),
          flush=True)
    if failures:
        print(f'not killed: {", ".join(failures)}', file=sys.stderr)
        return 1
    print(f'{len(selected)} mutants killed and restored', flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())

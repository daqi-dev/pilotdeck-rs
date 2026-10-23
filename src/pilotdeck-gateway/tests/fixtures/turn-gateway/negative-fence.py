#!/usr/bin/env python3
"""Run exclusively in this tree; use only the launcher's existing target.

Each mutation must fail at its named assertion, never at compile or timeout.
Filter controls intentionally run zero matching cases and must fail closed.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

fixture = Path(__file__).resolve().parent
workspace = fixture.parents[3]
repo = workspace.parent
source = workspace / 'pilotdeck-gateway/src/runtime/turn/gateway.rs'
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
env = dict(os.environ, CARGO_NET_OFFLINE='true')
assert Path(env['CARGO_TARGET_DIR']).resolve() == workspace / 'target'
assert env.get('PDRS_BASELINE'), 'set PDRS_BASELINE'
env.pop('PDRS_GATEWAY_CASE', None)
original = source.read_bytes()
text = original.decode()
start = text.index('            let wait_for = self\n')
end = text.index('            if let Some(mut done) = wait_for {', start)
lookup = text[start:end]
owner_start = text.index('            self.0\n                .submitter\n                .owner\n                .abort_turn(key, run, reason)')
owner_end = text.index('            // InProcessGateway.abortTurn waits by session', owner_start)
owner = text[owner_start:owner_end]
assert text.count(lookup) == text.count(owner) == 1
session_check = '.is_some_and(|submitted| &submitted.session_key == key)'
assert lookup.count(session_check) == 1
mutants = [
    ('snapshot-before-abort', text.replace(lookup, '').replace(owner, lookup + owner),
     'abort fence: "replacement-during-abort"'),
    ('active-run-filter', text.replace(lookup, lookup.replace(session_check,
        '.is_some_and(|submitted| &submitted.session_key == key && Some(submitted.run_id.clone()) == self.0.submitter.owner.active_turn_run_id(key))')),
     'abort fence: "admission-cleared-before-consumer"'),
    ('stale-run-check-skipped', text.replace('if active.as_ref() != Some(run) && !matching {', 'if false && active.as_ref() != Some(run) && !matching {'),
     'abort fence: \"stale-run-does-not-wait\"'),
    ('requested-run-filter', text.replace(lookup, lookup.replace(session_check,
        '.is_some_and(|submitted| &submitted.session_key == key && run.is_none_or(|run| run == &submitted.run_id))')),
     'abort fence: "replacement-during-abort"'),
    ('oldest-session-fence', text[:start] + text[start:].replace('.max_by_key(|(id, _)| *id)', '.min_by_key(|(id, _)| *id)', 1),
     'abort fence: \"latest-session-fence-only\"'),
]
command = ['cargo', 'test', '--offline', '--manifest-path', str(workspace / 'Cargo.toml'),
           '-p', 'pilotdeck-gateway', '--lib']
summary = []
lock = out / 'negative.lock'
lock.mkdir()

def run(name, test, modified=original, marker=None, case=None):
    assert source.read_bytes() == original, 'source changed outside this run'
    slots = subprocess.run(['bash', str(repo / 'scripts/cargo-slots.sh')], capture_output=True, text=True)
    while slots.returncode == 1:
        time.sleep(20)
        slots = subprocess.run(['bash', str(repo / 'scripts/cargo-slots.sh')], capture_output=True, text=True)
    assert slots.returncode == 0
    if modified != original:
        source.write_bytes(modified)
    run_env = dict(env)
    if case is not None:
        run_env['PDRS_GATEWAY_CASE'] = case
    try:
        result = subprocess.run(command + [test, '--', '--nocapture'], cwd=repo,
                                env=run_env, capture_output=True, text=True, timeout=300)
        raw = result.stdout + result.stderr
    finally:
        current = source.read_bytes()
        if current == modified:
            if modified != original:
                source.write_bytes(original)
        else:
            raise AssertionError('concurrent edit preserved; source requires manual inspection')
    (out / f'{name}.log').write_text(slots.stdout + slots.stderr + raw + f'\nexit={result.returncode}\n')
    valid = (result.returncode == 0 and '1 passed' in raw) if marker is None else (
        result.returncode == 101 and 'test result: FAILED' in raw and marker in raw
        and 'could not compile' not in raw)
    summary.append(dict(name=name, exit=result.returncode, valid=valid, marker=marker))
    (out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(f'{name}: {"PASS" if valid else "INVALID"} exit={result.returncode}', flush=True)
    assert valid, name

try:
    run('baseline', 'abort_waits_for_latest_real_ts_session_fence')
    for name, modified, marker in mutants:
        run(name, 'abort_waits_for_latest_real_ts_session_fence', modified.encode(), marker)
    for name, case in [('unknown-case', 'does-not-exist'), ('empty-case', '')]:
        run(name, 'real_ts_gateway_frames_registry_replay_and_concurrency',
            marker='PDRS_GATEWAY_CASE matched no scenario', case=case)
finally:
    lock.rmdir()
    assert source.read_bytes() == original, 'original bytes not restored'
    (out / 'restored-sha256.txt').write_text(hashlib.sha256(original).hexdigest() + '\n')

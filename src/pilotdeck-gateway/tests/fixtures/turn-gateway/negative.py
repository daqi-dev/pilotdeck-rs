#!/usr/bin/env python3
"""Mutate product code, replay unchanged real-TS gold, restore exact bytes.

Compile failures and timeouts are INVALID. Run exclusively in the worker tree.
Every selected replacement has one exact source anchor; no test/oracle is edited.
"""
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

fixture = Path(__file__).resolve().parent
workspace = fixture.parents[3]
repo = workspace.parent
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
turn = workspace / 'pilotdeck-gateway/src/runtime/turn'
gateway = turn / 'gateway.rs'
connection = workspace / 'pilotdeck-gateway/src/runtime/connection/mod.rs'
registry = workspace / 'pilotdeck-gateway/src/storage/run_registry.rs'

def mutation(name, path, before, after, case=None, marker='assertion'):
    return dict(name=name, changes=[(path, before, after)], case=case, marker=marker)

mutants = [
    mutation('transport-bypasses-serial-host', gateway, 'self.host\n            .call(async move { connection.receive(&text).await })\n            .await?', 'connection.receive(&text).await', 'permission-before-event-and-final', 'gateway host thread'),
    mutation('dropped-event', gateway, '                emit(event);', '                let _ = event;', 'permission-before-event-and-final'),
    mutation('duplicated-event', gateway, '                emit(event);', '                emit(event.clone()); emit(event);', 'permission-before-event-and-final'),
    mutation('early-final', gateway, '                emit(event);', '                emit(event); break;', 'permission-before-event-and-final'),
    mutation('registry-seq-used-as-ws-seq', connection, 'Mutex::new((0u64, None, None))', 'Mutex::new((1u64, None, None))', 'permission-before-event-and-final'),
    mutation('module-seq-used-as-ws-seq', connection, 'out.insert("seq".into(), JsValue::Number(stream.0 as f64));', 'out.insert("seq".into(), event.get("sequence").cloned().unwrap_or(JsValue::Number(stream.0 as f64)));', 'permission-before-event-and-final'),
    mutation('ws-seq-used-as-registry-seq', registry, 'record.insert("lastSeq".into(), JsValue::Number(0.0));', 'record.insert("lastSeq".into(), JsValue::Number(-1.0));', 'permission-before-event-and-final'),
    mutation('module-seq-used-as-registry-seq', registry, 'record.insert("lastSeq".into(), JsValue::Number(0.0));', 'record.insert("lastSeq".into(), JsValue::Number(41.0));', 'permission-before-event-and-final'),
    mutation('ws-seq-overwrites-module-seq', gateway, '                emit(event);', '                let mut event = event; if event.get("sequence").is_some() && let JsValue::Object(ref mut data) = event { data.insert("sequence".into(), JsValue::Number(0.0)); } emit(event);', 'permission-before-event-and-final'),
    mutation('registry-seq-overwrites-module-seq', gateway, '                emit(event);', '                let mut event = event; if event.get("sequence").is_some() && let JsValue::Object(ref mut data) = event { data.insert("sequence".into(), JsValue::Number(1.0)); } emit(event);', 'permission-before-event-and-final'),
    mutation('notification-dropped', connection, 'self.notify("permission_changed", Some(JsValue::Object(payload)));\n            }\n            let mut out', 'let _ = payload;\n            }\n            let mut out', 'permission-before-event-and-final'),
    mutation('final-seq-skipped', connection, 'out.insert("seq".into(), JsValue::Number(seq as f64));', 'out.insert("seq".into(), JsValue::Number((seq + 1) as f64));', 'permission-before-event-and-final'),
    mutation('final-marked-nonterminal', connection, 'out.insert("final".into(), JsValue::Bool(true));', 'out.insert("final".into(), JsValue::Bool(false));', 'permission-before-event-and-final'),
    mutation('completion-overrides-error', connection, 'error.or(completed)', 'completed.or(error)', 'error-before-success'),
    mutation('pending-interaction-not-preserved', gateway, 'Ok(result.get("preserveTurn") == Some(&JsValue::Bool(true)))', 'let _ = result; Ok(false)', 'close-preserves-pending'),
    mutation('close-binding-corrupted', gateway, '.disconnect(session, &binding)', '.disconnect(session, &InteractionBinding { generation: binding.generation + 1, ..binding })', 'close-preserves-pending'),
    mutation('submit-error-code-lost', gateway, 'code: error.code,', 'code: "lost_code".into(),', 'finally-error-response'),
    mutation('abort-consumer-fence-skipped', gateway, 'let _ = done.wait_for(|done| *done).await;', 'let _ = &mut done;', 'abort-generated-run-id'),
    mutation('expiry-runs-on-clock-thread', gateway, 'let _ = due.send((key.clone(), serial, callback.clone()));', 'let _ = (&due, &key, serial); callback();', None, 'serial timer:'),
    mutation('expiry-callback-dropped', gateway, 'if current {\n                    callback();\n                }', 'if current { let _ = callback; }', None, 'serial timer:'),
    mutation('queued-cancel-ignored', gateway, 'self.entries.lock().unwrap().remove(key);', 'let _ = self.entries.lock().unwrap().get(key);', None, 'cancel queued serial timer'),
    mutation('replacement-generation-ignored', gateway, 'entries.get(&key).is_some_and(|id| *id == serial)', 'entries.contains_key(&key)', None, 'replacement invalidates detached serial timer'),
    mutation('queued-dispose-ignored', gateway, 'self.entries.lock().unwrap().clear();', 'let _ = self.entries.lock().unwrap().len();', None, 'dispose cancels queued serial timers'),
]
# Change only ordering, keeping exactly one ordinary event and notification.
mutants.append(dict(name='notification-after-event', case='permission-before-event-and-final', marker='assertion', changes=[
    (connection, 'let emit = |event: JsValue| {', 'let emit = |event: JsValue| { let mut delayed_notification = None;'),
    (connection, 'self.notify("permission_changed", Some(JsValue::Object(payload)));\n            }\n            let mut out', 'delayed_notification = Some(JsValue::Object(payload));\n            }\n            let mut out'),
    (connection, 'self.send(JsValue::Object(out));\n            stream.0 += 1;', 'self.send(JsValue::Object(out)); if let Some(payload) = delayed_notification { self.notify("permission_changed", Some(payload)); }\n            stream.0 += 1;'),
]))
original = {path: path.read_bytes() for path in [gateway, connection, registry]}
for mutant in mutants:
    for path, before, after in mutant['changes']:
        assert original[path].decode().count(before) == 1, (mutant['name'], 'anchor not unique', before)
if sys.argv[2:] == ['--list']:
    print('\n'.join(m['name'] for m in mutants))
    raise SystemExit(0)
selected = [dict(name='baseline', changes=[], case=None, marker='test result: ok.')] + mutants
if len(sys.argv) > 2:
    selected = [m for m in selected if m['name'] in sys.argv[2:]]
    assert selected, 'unknown controls'
lock = repo / '_tmp/20261023-a72-pr5-gateway/negative.lock'
lock.mkdir()
summary = []
env = dict(os.environ, CARGO_NET_OFFLINE='true')
assert env.get('PDRS_BASELINE'), 'set PDRS_BASELINE'
env.setdefault('CARGO_TARGET_DIR', str(repo / '_build/target'))
command = ['cargo', 'test', '--offline', '--manifest-path', str(workspace / 'Cargo.toml'), '-p', 'pilotdeck-gateway', '--lib', 'runtime::turn::gateway::tests', '--', '--nocapture', '--test-threads=1']
(out / 'original-sha256.json').write_text(json.dumps({str(p.relative_to(repo)): hashlib.sha256(data).hexdigest() for p, data in original.items()}, indent=2)+'\n')
try:
    for mutant in selected:
        assert all(p.read_bytes() == data for p, data in original.items()), 'source changed outside this run'
        modified = dict(original)
        for path, before, after in mutant['changes']:
            modified[path] = modified[path].decode().replace(before, after, 1).encode()
        for path in original:
            if modified[path] != original[path]: path.write_bytes(modified[path])
        slots = subprocess.run(['bash', str(repo / 'scripts/cargo-slots.sh')], text=True, capture_output=True)
        while slots.returncode == 1:
            time.sleep(20)
            slots = subprocess.run(['bash', str(repo / 'scripts/cargo-slots.sh')], text=True, capture_output=True)
        assert slots.returncode == 0
        run_env = dict(env)
        if mutant['case']: run_env['PDRS_GATEWAY_CASE'] = mutant['case']
        else: run_env.pop('PDRS_GATEWAY_CASE', None)
        process = subprocess.Popen(command, cwd=repo, env=run_env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, start_new_session=True)
        timed_out = False
        try:
            raw, _ = process.communicate(timeout=180)
        except subprocess.TimeoutExpired:
            timed_out = True
            os.killpg(process.pid, signal.SIGKILL)
            raw, _ = process.communicate()
        finally:
            for path, data in original.items():
                assert path.read_bytes() == modified[path], f'concurrent source edit: {path}; preserve it'
                if modified[path] != data: path.write_bytes(data)
        valid = not timed_out and ((process.returncode == 0 and 'test result: ok. 3 passed' in raw) if mutant['name'] == 'baseline' else (
            process.returncode == 101 and 'test result: FAILED' in raw and 'could not compile' not in raw and 'assertion' in raw and mutant['marker'] in raw))
        (out / (mutant['name']+'.log')).write_text('COMMAND '+' '.join(command)+'\n'+slots.stdout+slots.stderr+raw+f'\nexit={process.returncode}\n')
        summary.append(dict(name=mutant['name'], case=mutant['case'], exit=process.returncode, marker=mutant['marker'], valid=valid))
        (out/'summary.json').write_text(json.dumps(summary, indent=2)+'\n')
        print(f'{mutant["name"]}: {"KILLED" if valid and mutant["changes"] else "PASS" if valid else "INVALID"} exit={process.returncode}', flush=True)
        if not valid: raise SystemExit(1)
finally:
    assert all(path.read_bytes() == data for path, data in original.items()), 'original bytes not restored'
    lock.rmdir()
print(f'{len(summary)} controls checked; original bytes restored', flush=True)

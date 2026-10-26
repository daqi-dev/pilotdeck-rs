#!/usr/bin/env python3
"""Mutate product code, replay unchanged real-TS gold, restore exact bytes.

Compile failures and timeouts are INVALID. Run exclusively in the worker tree.
Every selected replacement has one exact source anchor; no test/oracle is edited.
"""
import hashlib
import json
import os
import re
from pathlib import Path
import signal
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed
from negative_process import mutating_sources, run_owned

fixture = Path(__file__).resolve().parent
workspace = fixture.parents[3]
repo = workspace.parent
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
turn = workspace / 'pilotdeck-gateway/src/runtime/turn'
gateway = turn / 'gateway.rs'
connection = workspace / 'pilotdeck-gateway/src/runtime/connection/mod.rs'
registry = workspace / 'pilotdeck-gateway/src/storage/run_registry.rs'

GATEWAY = 'runtime::turn::gateway::tests::'
REPLAY = GATEWAY + 'real_ts_gateway_frames_registry_replay_and_concurrency'
TIMER = GATEWAY + 'serial_timer_cancels_queued_callbacks_and_replacements'
EMIT = GATEWAY + 'concurrent_external_emits_are_serialized_on_the_serial_host'
NOTIFY = GATEWAY + 'hosted_connection_notify_enters_the_serial_host'

def mutation(name, path, before, after, case=None, marker='assertion', test=REPLAY):
    # Every mutant names the test that must catch it; the runner uses that name
    # with --exact and requires both `... FAILED` and a marker in its panic
    # block (#813).
    return dict(name=name, changes=[(path, before, after)], case=case, marker=marker, test=test)

mutants = [
    mutation('transport-bypasses-serial-host', gateway, 'self.host\n            .call(async move { connection.receive(&text).await })\n            .await?', 'connection.receive(&text).await', 'permission-before-event-and-final', 'gateway host thread'),
    # close-active reaches the `Session::abort` host-thread assertion after
    # disconnect; running `closed` on the caller thread trips it there.
    mutation('closed-bypasses-serial-host', gateway, 'self.host\n            .call(async move { connection.closed().await })\n            .await?', 'connection.closed().await', 'close-active', 'gateway host thread'),
    # `notify` only sends a frame, so it needs its own host-thread observation:
    # the fixture transport records the thread of every outbound frame.
    mutation('notify-bypasses-serial-host', gateway, 'self.host\n            .call(async move {\n                connection.notify(&name, payload);\n            })\n            .await', '{ connection.notify(&name, payload); Ok(()) }', None, 'A7.2-COORDINATOR-DELIVERY-001-R1: HostedConnection::notify must run on the serial host thread', test=NOTIFY),
    mutation('emit-bypasses-serial-host', gateway, 'let gateway = self.gateway.clone();\n        self.call(async move { gateway.0.submitter.coordinator.emit(&session, &event) })\n            .await', 'Ok(self.gateway.0.submitter.coordinator.emit(&session, &event))', None, 'A7.2-COORDINATOR-DELIVERY-001-R1', test=EMIT),
    # Delivery moves to a later host task: same thread, non-overlapping, once
    # per event, but the `emit` future now resolves before the sink ran.
    mutation('emit-deferred-after-return', gateway, 'let gateway = self.gateway.clone();\n        self.call(async move { gateway.0.submitter.coordinator.emit(&session, &event) })\n            .await', 'let gateway = self.gateway.clone();\n        let jobs = self.jobs.clone();\n        let _ = jobs.send(Box::pin(async move {\n            tokio::time::sleep(std::time::Duration::from_millis(250)).await;\n            gateway.0.submitter.coordinator.emit(&session, &event);\n        }));\n        Ok(true)', None, 'A7.2-COORDINATOR-DELIVERY-001-R1: emit returned before its own event reached the sink', test=EMIT),
    # Every event reaches the sink twice, on the host, before its call returns.
    # A total-only count also sees 16 here; the named per-event assertion must
    # name the duplicated event instead of only reporting a wrong total.
    mutation('emit-duplicated-delivery', gateway, 'let gateway = self.gateway.clone();\n        self.call(async move { gateway.0.submitter.coordinator.emit(&session, &event) })\n            .await', 'let gateway = self.gateway.clone();\n        self.call(async move {\n            let accepted = gateway.0.submitter.coordinator.emit(&session, &event);\n            gateway.0.submitter.coordinator.emit(&session, &event);\n            accepted\n        })\n        .await', None, 'A7.2-COORDINATOR-DELIVERY-001-R1: event concurrent-0 must reach the sink exactly once', test=EMIT),
    mutation('dropped-event', gateway, '                emit(event);', '                let _ = event;', 'permission-before-event-and-final', 'transport observation did not arrive'),
    mutation('duplicated-event', gateway, '                emit(event);', '                emit(event.clone()); emit(event);', 'permission-before-event-and-final'),
    mutation('early-final', gateway, '                emit(event);', '                emit(event); break;', 'permission-before-event-and-final'),
    mutation('registry-seq-used-as-ws-seq', connection, 'Mutex::new((0u64, None, None))', 'Mutex::new((1u64, None, None))', 'permission-before-event-and-final'),
    mutation('module-seq-used-as-ws-seq', connection, 'out.insert("seq".into(), JsValue::Number(stream.0 as f64));', 'out.insert("seq".into(), event.get("sequence").cloned().unwrap_or(JsValue::Number(stream.0 as f64)));', 'permission-before-event-and-final'),
    mutation('ws-seq-used-as-registry-seq', registry, 'record.insert("lastSeq".into(), JsValue::Number(0.0));', 'record.insert("lastSeq".into(), JsValue::Number(-1.0));', 'permission-before-event-and-final'),
    mutation('module-seq-used-as-registry-seq', registry, 'record.insert("lastSeq".into(), JsValue::Number(0.0));', 'record.insert("lastSeq".into(), JsValue::Number(41.0));', 'permission-before-event-and-final'),
    mutation('ws-seq-overwrites-module-seq', gateway, '                emit(event);', '                let mut event = event; if event.get("sequence").is_some() && let JsValue::Object(ref mut data) = event { data.insert("sequence".into(), JsValue::Number(0.0)); } emit(event);', 'permission-before-event-and-final'),
    mutation('registry-seq-overwrites-module-seq', gateway, '                emit(event);', '                let mut event = event; if event.get("sequence").is_some() && let JsValue::Object(ref mut data) = event { data.insert("sequence".into(), JsValue::Number(1.0)); } emit(event);', 'permission-before-event-and-final'),
    mutation('notification-dropped', connection, 'self.notify("permission_changed", Some(JsValue::Object(payload)));\n            }\n            let mut out', 'let _ = payload;\n            }\n            let mut out', 'permission-before-event-and-final', 'transport observation did not arrive'),
    mutation('final-seq-skipped', connection, 'out.insert("seq".into(), JsValue::Number(seq as f64));', 'out.insert("seq".into(), JsValue::Number((seq + 1) as f64));', 'permission-before-event-and-final'),
    mutation('final-marked-nonterminal', connection, 'out.insert("final".into(), JsValue::Bool(true));', 'out.insert("final".into(), JsValue::Bool(false));', 'permission-before-event-and-final'),
    mutation('completion-overrides-error', connection, 'error.or(completed)', 'completed.or(error)', 'error-before-success'),
    mutation('pending-interaction-not-preserved', gateway, 'Ok(result.get("preserveTurn") == Some(&JsValue::Bool(true)))', 'let _ = result; Ok(false)', 'close-preserves-pending'),
    mutation('close-binding-corrupted', gateway, '.disconnect(session, &binding)', '.disconnect(session, &InteractionBinding { generation: binding.generation + 1, ..binding })', 'close-preserves-pending'),
    mutation('submit-error-code-lost', gateway, 'code: error.code,', 'code: "lost_code".into(),', 'finally-error-response'),
    mutation('abort-consumer-fence-skipped', gateway, 'let _ = done.wait_for(|done| *done).await;', 'let _ = &mut done;', 'abort-generated-run-id'),
    # These run the timer test, whose assertions are not the replay test's
    # "serial timer: <case>" message. Anchor each on the assertion that its
    # selected test actually trips (#813 fix-r1).
    mutation('expiry-runs-on-clock-thread', gateway, 'let _ = due.send((key.clone(), serial, callback.clone()));', 'let _ = (&due, &key, serial); callback();', None, 'cancel queued serial timer', test=TIMER),
    mutation('expiry-callback-dropped', gateway, 'if current {\n                    callback();\n                }', 'if current { let _ = callback; }', None, 'replacement timer fires once', test=TIMER),
    mutation('queued-cancel-ignored', gateway, 'self.entries.lock().unwrap().remove(key);', 'let _ = self.entries.lock().unwrap().get(key);', None, 'cancel queued serial timer', test=TIMER),
    mutation('replacement-generation-ignored', gateway, 'entries.get(&key).is_some_and(|id| *id == serial)', 'entries.contains_key(&key)', None, 'replacement invalidates detached serial timer', test=TIMER),
    mutation('queued-dispose-ignored', gateway, 'self.entries.lock().unwrap().clear();', 'let _ = self.entries.lock().unwrap().len();', None, 'dispose cancels queued serial timers', test=TIMER),
]
# Each new invalid abort-argument behavior is checked through unchanged TS gold.
for name, case in [
    ('missing-abort-session-rejected', 'abort-missing-session'),
    ('nonstring-abort-session-rejected', 'abort-numeric-session'),
]:
    mutants.append(mutation(name, gateway,
        'let Some(key) = params.get("sessionKey").and_then(JsValue::as_string) else {\n                return Ok(());\n            };',
        'let key = params.get("sessionKey").and_then(JsValue::as_string).ok_or_else(|| RequestError::new("sessionKey must be a string."))?;', case))
for name, case in [
    ('nonstring-abort-run-as-session-abort', 'abort-numeric-run'),
    ('null-abort-run-as-session-abort', 'abort-null-run'),
]:
    mutants.append(mutation(name, gateway,
        '.is_some_and(|run| !matches!(run, JsValue::String(_) | JsValue::Undefined))',
        '.is_some_and(|run| { let _ = run; false })', case))
# Change only ordering, keeping exactly one ordinary event and notification.
mutants.append(dict(name='notification-after-event', case='permission-before-event-and-final', marker='assertion', test=REPLAY, changes=[
    (connection, 'let emit = |event: JsValue| {', 'let emit = |event: JsValue| { let mut delayed_notification = None;'),
    (connection, 'self.notify("permission_changed", Some(JsValue::Object(payload)));\n            }\n            let mut out', 'delayed_notification = Some(JsValue::Object(payload));\n            }\n            let mut out'),
    (connection, 'self.send(JsValue::Object(out));\n            stream.0 += 1;', 'self.send(JsValue::Object(out)); if let Some(payload) = delayed_notification { self.notify("permission_changed", Some(payload)); }\n            stream.0 += 1;'),
]))
with mutating_sources(repo, [gateway, connection, registry]) as snapshots:
    original = {path: snapshots[path] for path in [gateway, connection, registry]}
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
    summary = []
    env = dict(os.environ, CARGO_NET_OFFLINE='true')
    assert env.get('PDRS_BASELINE'), 'set PDRS_BASELINE'
    target_dir = Path(env['CARGO_TARGET_DIR']).resolve()
    assert target_dir in {(workspace / 'target').resolve(), (repo / '_build/cargo-target').resolve()}, \
        'use this tree src/target or _build/cargo-target'
    # Current non-ignored tests selected by the gateway module filter. New tests may
    # increase the total; each existing behavior must still execute successfully.
    baseline_tests = [
        'transport_observation_waits_for_delivery_and_receive_completion',
        'real_ts_gateway_frames_registry_replay_and_concurrency',
        'serial_retention_matches_real_ts_host_steps_and_dispose',
        'serial_timer_cancels_queued_callbacks_and_replacements',
        'abort_waits_for_latest_real_ts_session_fence',
        'abort_snapshots_before_ungated_cleanup_replacement',
    'concurrent_external_emits_are_serialized_on_the_serial_host',
        'hosted_connection_notify_enters_the_serial_host',
    ]
    base_command = ['cargo', 'test', '--offline', '--manifest-path', str(workspace / 'Cargo.toml'),
                    '-p', 'pilotdeck-gateway', '--lib']
    (out / 'original-sha256.json').write_text(json.dumps({str(p.relative_to(repo)): hashlib.sha256(data).hexdigest() for p, data in original.items()}, indent=2)+'\n')
    try:
        for mutant in selected:
            assert all(p.read_bytes() == data for p, data in original.items()), 'source changed outside this run'
            slots = subprocess.run(['bash', str(repo / 'scripts/cargo-slots.sh')], text=True, capture_output=True, timeout=10)
            slot_deadline = time.monotonic() + 1800
            while slots.returncode == 1:
                assert time.monotonic() < slot_deadline, 'cargo slots unavailable for 30 minutes'
                time.sleep(20)
                slots = subprocess.run(['bash', str(repo / 'scripts/cargo-slots.sh')], text=True, capture_output=True, timeout=10)
            assert slots.returncode == 0
            modified = dict(original)
            for path, before, after in mutant['changes']:
                modified[path] = modified[path].decode().replace(before, after, 1).encode()
            for path in original:
                if modified[path] != original[path]: path.write_bytes(modified[path])
            run_env = dict(env)
            # The baseline proof may run against a temporary alternate git dir
            # (a commit-less sandbox cannot write the shared worktree git dir);
            # cargo and its build scripts must still see the real worktree.
            run_env.pop('GIT_DIR', None)
            run_env.pop('GIT_WORK_TREE', None)
            if mutant['case']: run_env['PDRS_GATEWAY_CASE'] = mutant['case']
            else: run_env.pop('PDRS_GATEWAY_CASE', None)
            timed_out = False
            if mutant['name'] == 'baseline':
                # The module filter still proves every existing test is green.
                command = base_command + [GATEWAY, '--', '--nocapture', '--test-threads=1']
            else:
                # #813: run the one named test and require its own FAILED line
                # plus a marker inside its own panic block.
                command = base_command + [mutant['test'], '--', '--exact', '--nocapture', '--test-threads=1']
            try:
                run = run_owned(command, cwd=repo, env=run_env, timeout=180)
                raw = run.stdout + run.stderr
                returncode = run.returncode
            except subprocess.TimeoutExpired as error:
                timed_out = True
                raw = (error.output or '') + (error.stderr or '')
                returncode = -signal.SIGKILL
            finally:
                for path, data in original.items():
                    assert path.read_bytes() == modified[path], f'concurrent source edit: {path}; preserve it'
                    if modified[path] != data: path.write_bytes(data)
            result = re.search(r'^test result: ok\. (\d+) passed; 0 failed;', raw, re.MULTILINE)
            baseline_ok = (returncode == 0 and result is not None
                and int(result.group(1)) >= len(baseline_tests)
                and all(re.search(r'^test runtime::turn::gateway::tests::' + re.escape(name) + r' \.\.\. ok$',
                    raw, re.MULTILINE) for name in baseline_tests))
            valid = not timed_out and (baseline_ok if mutant['name'] == 'baseline' else (
                returncode == 101 and 'test result: FAILED' in raw and 'could not compile' not in raw
                and named_test_killed(raw, mutant['test'], mutant['marker'])))
            (out / (mutant['name']+'.log')).write_text('COMMAND '+' '.join(command)+'\n'+slots.stdout+slots.stderr+raw+f'\nexit={returncode}\n')
            summary.append(dict(name=mutant['name'], case=mutant['case'], test=mutant.get('test'),
                                exit=returncode, marker=mutant['marker'], valid=valid))
            (out/'summary.json').write_text(json.dumps(summary, indent=2)+'\n')
            print(f'{mutant["name"]}: {"KILLED" if valid and mutant["changes"] else "PASS" if valid else "INVALID"} exit={returncode}', flush=True)
            # Retain every control even after an INVALID result.
    finally:
        assert all(path.read_bytes() == data for path, data in original.items()), 'original bytes not restored'
    print(f'{len(summary)} controls checked; original bytes restored', flush=True)
    raise SystemExit(0 if all(row['valid'] for row in summary) else 1)

#!/usr/bin/env python3
"""Commit first. --sample runs two mutants; every cargo owns its process tree."""
import json
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
root = Path(__file__).resolve().parents[5]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR'):
    raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
source = root / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
runner = root / 'src/pilotdeck-gateway/src/runtime/turn/runner.rs'
tests = root / 'src/pilotdeck-gateway/src/runtime/turn/gates_tests.rs'
consumer = root / 'src/pilotdeck-gateway/tests/fixtures/turn-runner/oracle.json'
factory_peer = root / 'src/pilotdeck-gateway/tests/fixtures/host-factory/peer.py'
factory = root / 'src/pilotdeck-gateway/src/runtime/turn/host_factory.rs'
factory_tests = root / 'src/pilotdeck-gateway/src/runtime/turn/host_factory_tests.rs'
command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway', '--lib']
def run(name, test='gates_tests'):
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
    (out / (name + '.slots.log')).write_text(slots.stdout + slots.stderr)
    result = run_owned(command + [test], cwd=root, env=env, timeout=900)
    (out / (name + '.log')).write_text(result.stdout + result.stderr)
    return result
lazy = 'host = async { host_factory(request.identity.clone(), recorder.clone()).await } => host?,'
mutants = [
    ('factory-close-timeout', factory, '.with_close_timeout(close_timeout)', '.with_close_timeout({ let _ = close_timeout; Limits::default().close_timeout })', 'live_prepared_cleanup_uses_close_timeout'),
    ('prepared-close-timeout', source, 'TurnScope::new(inner.clone(), close_timeout)', 'TurnScope::new(inner.clone(), { let _ = close_timeout; std::time::Duration::from_secs(5) })', 'live_prepared_cleanup_uses_close_timeout'),
    ('consumer-exists', consumer, '"test": "runner_success"', '"test": "runner_nonexistent_consumer"', 'oracle_consumers_exist'),
    ('skip-start', runner, 'self.io(|r, s, t| r.start_turn(s, t)).await?;', 'let _ = self.io(|r, s, t| r.start_turn(s, t)).await;', 'gates_start_failure'),
    ('eager-connect', source, 'if execute_wait.await.is_err() {', 'let client = client.await;\n                let client: HostFuture<HostClient> = Box::pin(async move { client });\n                if execute_wait.await.is_err() {', 'gates_accept_failure'),
    ('skip-accept', runner, 'r.writer()\n                    .record_accepted_input(s, t, written, input.metadata)?', '{ let _ = (r, s, t, written, input.metadata); }', 'gates_accept_failure'),
    ('skip-complete', runner, 'self.io(move |r, s, t| r.complete_turn(s, t, written)).await', 'self.io(move |_, _, _| { drop(written); Ok(()) }).await', 'gates_cancel_ok'),
    ('skip-failure-complete', runner, 'self.io(move |r, s, t| r.complete_turn(s, t, written))\n            .await?;', 'self.io(move |_, _, _| { drop(written); Ok(()) })\n            .await?;', 'gates_error_ok'),
    ('release-pending-slot', runner, 'let owner = self.owner.clone();', 'let owner: Option<Arc<dyn Send + Sync>> = None;', 'gates_drop_pending'),
    ('discard-host-failure', source, 'Err(error) if self.accepted =>', 'Err(error) if self.accepted && false =>', 'gates_protocol_ok'),
    ('original-success-after-fallback', source, 'events.len() != 1', 'false', 'gates_cancel_once'),
    ('omit-prepared-cleanup', source, 'let _ = recording_sender.send(Recording::new(runner, owner));', 'pending_host.handoff();\n                let _ = recording_sender.send(Recording::new(runner, owner));', 'gates_start_failure'),
    ('factory-ready-priority', source, 'biased;', 'biased; ' + lazy, 'gates_factory_ready_cancel'),
    ('duplicate-normal-terminal', source, 'return self.deliver_recorded(event);\n        }\n        if terminal {\n            self.release();', 'return self.deliver_recorded(event);\n        }\n        if terminal {\n            self.release(); self.recorded.push_back(event.clone());', 'gates_cancel_ok'),
    ('sidecar-lifecycle-leak', source, 'if event_type.equals_str("turn_started") || event_type.equals_str("input_accepted") {', 'if false && (event_type.equals_str("turn_started") || event_type.equals_str("input_accepted")) {', 'host_lifecycle_cannot_justify_a_sidecar_final'),
    ('duplicate-host-terminal', source, 'if event.get("type") == Some(&"turn_completed".into()) {', 'if event.get("type") == Some(&"turn_completed".into()) { self.recorded.push_back(event.clone());', 'gates_cancel_once'),
    ('multi-turn-peer-marker', factory_peer, 'f"{pid_file}.{os.getpid()}.execute"', 'pid_file + ".execute"', 'live_session_shares_recorder_and_reads_current_history'),
]
# Include the new test source in the baseline guard. Before submission it is
# absent from HEAD: refuse rather than manufacture a second repository/target.
with mutating_sources(root, [source, runner, tests, consumer, factory_peer, factory_tests, factory]) as originals:
    assert run('positive').returncode == 0
    assert run('positive-consumers', 'oracle_consumers_exist').returncode == 0
    selected = set(sys.argv[2:])
    assert selected <= {'--sample'} | {m[0] for m in mutants}
    chosen = [m for m in mutants if m[0] in {'skip-start', 'eager-connect'}] if '--sample' in selected else [m for m in mutants if not selected or m[0] in selected]
    for name, path, old, new, test in chosen:
        body = originals[path].decode()
        if name == 'omit-prepared-cleanup':
            assert body.count('pending_host.handoff();') == 1
            body = body.replace('pending_host.handoff();', '', 1)
        if name == 'factory-ready-priority':
            assert body.count(lazy) == 1
            body = body.replace(lazy, '', 1)
            # Only the first select belongs to factory/cancel arbitration.
            body = body.replace(old, new, 1)
        else:
            assert body.count(old) == 1, (name, body.count(old))
            body = body.replace(old, new, 1)
        path.write_text(body)
        result = run(name, test)
        assert result.returncode != 0 and 'test result: FAILED' in result.stdout, name
        assert f'::{test} ... FAILED' in result.stdout and 'panicked at' in result.stdout + result.stderr, name
        if name == 'factory-close-timeout':
            assert 'factory forwards close_timeout' in result.stdout, name
        if name == 'prepared-close-timeout':
            assert 'configured deadline' in result.stdout or 'configured budget' in result.stdout, name
        path.write_bytes(originals[path])
        assert run(name + '-restored', test).returncode == 0
    (out / 'summary.json').write_text(json.dumps({'mutants': len(chosen), 'restored': True}) + '\n')

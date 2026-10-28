#!/usr/bin/env python3
"""Commit first. --sample runs two mutants; every cargo owns its process tree."""
import json
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, run_cases, matrix_main
root = Path(__file__).resolve().parents[5]
compile_only = '--compile-only' in sys.argv[1:]
sys.argv = [arg for arg in sys.argv if arg != '--compile-only']
out = Path(sys.argv[1]).resolve()
if not compile_only:
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
stream_tests = root / 'src/pilotdeck-gateway/src/runtime/turn/live_stream_tests.rs'
lazy = 'host = async { host_factory(request.identity.clone(), recorder.clone(), signal).await } => host?,'
mutants = [
    ('factory-close-timeout', factory, '.with_close_timeout(close_timeout)', '.with_close_timeout({ let _ = close_timeout; Limits::default().close_timeout })', 'live_prepared_cleanup_uses_close_timeout'),
    ('prepared-close-timeout', source, 'TurnScope::new(inner.clone(), close_timeout)', 'TurnScope::new(inner.clone(), { let _ = close_timeout; std::time::Duration::from_secs(5) })', 'live_prepared_cleanup_uses_close_timeout'),
    ('consumer-exists', consumer, '"test": "runner_success"', '"test": "runner_nonexistent_consumer"', 'oracle_consumers_exist'),
    ('skip-start', runner, 'self.io(|r, s, t| r.start_turn(s, t)).await?;', 'let _ = self.io(|r, s, t| r.start_turn(s, t)).await;', 'gates_start_failure'),
    ('eager-connect', source, 'let Ok(messages) = execute_wait.await else {', 'let client = client.await;\n                let client: HostFuture<HostClient> = Box::pin(async move { client });\n                let Ok(messages) = execute_wait.await else {', 'gates_accept_failure'),
    ('skip-accept', runner, 'r.writer()\n                    .record_accepted_input(s, t, written, input.metadata)?', '{ let _ = (r, s, t, written, input.metadata); }', 'gates_accept_failure'),
    ('skip-complete', runner, 'self.io(move |r, s, t| r.complete_turn(s, t, written)).await', 'self.io(move |_, _, _| { drop(written); Ok(()) }).await', 'gates_cancel_ok'),
    ('skip-failure-complete', runner, 'self.io(move |r, s, t| r.complete_turn(s, t, written))\n            .await?;', 'self.io(move |_, _, _| { drop(written); Ok(()) })\n            .await?;', 'gates_error_ok'),
    ('release-pending-slot', runner, 'let owner = self.owner.clone();', 'let owner: Option<Arc<dyn Send + Sync>> = None;', 'gates_drop_pending'),
    ('discard-host-failure', source, 'Err(error) if self.accepted =>', 'Err(error) if self.accepted && false =>', 'gates_protocol_ok'),
    ('original-success-after-fallback', source, 'events.len() != 1', 'false', 'gates_cancel_once'),
    ('omit-prepared-cleanup', source, 'let _ = recording_sender.send(Recording::new(runner, owner));', 'pending_host.handoff();\n                let _ = recording_sender.send(Recording::new(runner, owner));', 'gates_start_failure'),
    ('factory-ready-priority', source, 'biased;', 'biased; ' + lazy, 'gates_factory_ready_cancel'),
    ('duplicate-normal-terminal', source, 'return self.deliver_recorded(event);\n        }\n        if terminal {\n            self.release();', 'return self.deliver_recorded(event);\n        }\n        if terminal {\n            self.release(); self.recorded.push_back(event.clone());', 'gates_cancel_ok'),
    ('sidecar-lifecycle-leak', source, '.any(|kind| event_type.equals_str(kind))', '.any(|_| false)', 'host_lifecycle_cannot_justify_a_sidecar_final'),
    ('duplicate-host-terminal', source, 'if event.get("type") == Some(&"turn_completed".into()) {', 'if event.get("type") == Some(&"turn_completed".into()) { self.recorded.push_back(event.clone());', 'gates_cancel_once'),
    ('multi-turn-peer-marker', factory_peer, 'f"{pid_file}.{os.getpid()}.execute"', 'pid_file + ".execute"', 'live_session_shares_recorder_and_reads_current_history'),
]
# The shared executor records every selected outcome and restores each source
# before continuing, including when an anchor or a baseline test fails.
stream = 'runtime::turn::live::stream_tests::'
def full_test(test):
    if test.startswith('gates_'):
        return stream + 'gates_tests::' + test
    if test == 'oracle_consumers_exist':
        return 'runtime::turn::runner::tests::' + test
    if test == 'host_lifecycle_cannot_justify_a_sidecar_final':
        return 'runtime::turn::live::tests::' + test
    if test == 'live_session_shares_recorder_and_reads_current_history':
        return 'runtime::turn::host_factory::tests::' + test
    return stream + test

assertions = {
    'factory-close-timeout': 'factory forwards close_timeout',
    'prepared-close-timeout': ['cleanup must stop at the configured deadline', 'cleanup must retain the configured budget'],
    'consumer-exists': 'missing Rust consumer: runner_nonexistent_consumer',
    'skip-start': '#743 gates start failure emits no event',
    'eager-connect': '#743 gates failed persistence never connects',
    'skip-accept': '#743 gates execute count',
    'skip-complete': 'complete count',
    'skip-failure-complete': 'complete count',
    'release-pending-slot': 'worker owns slot after drop',
    'discard-host-failure': '#743 gates complete count',
    'original-success-after-fallback': '#743 gates result kind',
    'omit-prepared-cleanup': 'handler cleanup exactly once',
    'factory-ready-priority': '#743 gates pre-cancel skips factory',
    'duplicate-normal-terminal': '#743 turn-only terminal once',
    'sidecar-lifecycle-leak': 'host lifecycle leak must be rejected',
    'duplicate-host-terminal': '#743 turn-only terminal once',
    'multi-turn-peer-marker': '#741k every history turn must execute successfully in its own sidecar',
}

def mutate(body, name, replacements):
    if name == 'omit-prepared-cleanup':
        assert body.count('pending_host.handoff();') == 1, '#743 prepared handoff anchor'
        body = body.replace('pending_host.handoff();', '', 1)
    if name == 'factory-ready-priority':
        assert body.count(lazy) == 1, '#743 factory branch anchor'
        body = body.replace(lazy, '', 1)
    for old, new in replacements:
        if name == 'factory-ready-priority':
            assert old in body, '#743 cancellation select anchor'
        else:
            assert body.count(old) == 1, (name, body.count(old))
        body = body.replace(old, new, 1)
    return body

selected = set(sys.argv[2:])
unknown = selected - {'--sample'} - {m[0] for m in mutants}
if unknown:
    raise SystemExit(f'unknown mutations: {sorted(unknown)}')
chosen = [m for m in mutants if m[0] in {'skip-start', 'eager-connect'}] if '--sample' in selected else [m for m in mutants if not selected or m[0] in selected]
guarded = [source, runner, tests, consumer, factory_peer, factory_tests, factory, stream_tests]
if compile_only:
    rows = []
    for name, path, old, new, test in chosen:
        original = path.read_text()
        rows.append((name, path, original, mutate(original, name, [(old, new)]), full_test(test), name))
    raise SystemExit(matrix_main(root, __file__, '', rows, passed_count=1, child_count=0,
                                paths=guarded, exact=True, args=['--compile-only', str(out)]))
try:
    with mutating_sources(root, guarded):
        result = run_cases(root,
            [Case(name, path, old, new, full_test(test), assertions[name], name)
             for name, path, old, new, test in chosen], out, env, mutate)
except Exception as error:
    result = 1
    reason = f'source guard/restoration: {type(error).__name__}: {error}'
    (out / 'summary.json').write_text(json.dumps({
        'controls': [dict(mutation=name, status='INVALID', reason=reason) for name, *_ in chosen],
        'counts': dict(KILLED=0, SURVIVED=0, INVALID=len(chosen)),
    }, indent=2) + '\n')
    print(reason, flush=True)
raise SystemExit(result)

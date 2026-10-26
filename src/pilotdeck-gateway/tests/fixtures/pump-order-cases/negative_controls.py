#!/usr/bin/env python3
"""Run one selected #855 pump-order mutation per invocation on a committed, idle product tree.

`gate-order` attaches the pump before the #832 durable gates; `wake-recheck`
removes the post-wake `stopped` re-check at live.rs:885. Both must turn their
paired #855 ordering test red with a test panic, not a compile failure.
"""
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
assert os.environ.get('CARGO_TARGET_DIR'), 'caller must set CARGO_TARGET_DIR'
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
path = root / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
mutations = {
    'gate-order': (
        '''                // No connection or execute can start until both durable gates commit.
                if execute_wait.await.is_err() {
                    return Ok(ExecuteResult::Aborted { terminal: None });
                }
                let client = tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return Ok(ExecuteResult::Aborted { terminal: None }),
                    client = client => client?,
                };
                pending_host.handoff();
                let identity = request.identity.clone();
                handler
                    .with_pump(
                        client.execute_cancellable(request, handler.clone(), cancellation),
                        identity,
                    )
                    .await''',
        '''                // No connection or execute can start until both durable gates commit.
                let identity = request.identity.clone();
                let gated = handler.clone();
                let gated_request = request;
                let gated_cancellation = cancellation;
                let gated_pending_host = pending_host;
                handler
                    .with_pump(
                        Box::pin(async move {
                            if execute_wait.await.is_err() {
                                return Ok(ExecuteResult::Aborted { terminal: None });
                            }
                            let client = tokio::select! {
                                biased;
                                _ = gated_cancellation.cancelled() => return Ok(ExecuteResult::Aborted { terminal: None }),
                                client = client => client?,
                            };
                            gated_pending_host.handoff();
                            client
                                .execute_cancellable(gated_request, gated.clone(), gated_cancellation)
                                .await
                        }),
                        identity,
                    )
                    .await''',
        'gates_pump_starts_only_after_the_gate_commits'),
    'wake-recheck': (
        '                    if self.forwarding.lock().unwrap().stopped || Instant::now() < self.forwarding.lock().unwrap().deadline() { continue; }',
        '                    if Instant::now() < self.forwarding.lock().unwrap().deadline() { continue; }',
        'host_pump_final_orders_behind_the_reserved_slot'),
}
name = sys.argv[2]
old, new, test = mutations[name]
def run(label):
    slot = wait_for_slot(root / 'scripts/cargo-slots.sh')
    (out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
    result = run_owned(['cargo', 'test', '--offline', '-q', '-p', 'pilotdeck-gateway', '--lib', test], cwd=root / 'src', env=os.environ.copy(), timeout=900)
    (out / f'{label}.log').write_text(result.stdout + result.stderr)
    return result
with mutating_sources(root, [path]) as snapshots:
    content = snapshots[path].decode()
    assert content.count(old) == 1, (name, content.count(old))
    assert run('positive').returncode == 0
    path.write_text(content.replace(old, new))
    red = run('mutant')
    assert red.returncode != 0 and 'test result: FAILED' in red.stdout and 'panicked at' in red.stdout, 'must fail a test assertion, not compilation'
assert run('restored').returncode == 0
print(f'PASS {name}: positive / assertion failure / restored')

#!/usr/bin/env python3
"""Run one selected mutation per invocation on a committed, idle product tree."""
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
    'idle': ('Duration::from_millis(500)', 'Duration::from_millis(501)', 'idle_reset_and_heartbeat_oracle'),
    'reset': ('if !module {', 'if false {', 'idle_reset_and_heartbeat_oracle'),
    'heartbeat': ('Duration::from_millis(2000)', 'Duration::from_millis(2500)', 'idle_reset_and_heartbeat_oracle'),
    'interleave': ('self.subagents.retain(|s| s.id != id);', '// keep completed subagent', 'idle_reset_and_heartbeat_oracle'),
    'module-phase': ('if !module {', 'if true {', 'pending_callback_does_not_hold_batches'),
    'stale-stop': ('if heartbeat && state.stopped {', 'if false {', 'stops_before_dispose_closes_port'),
    'wire-buffer': ('if payload.is_some() {', 'if false {', 'wire_predecessor_cancel_and_drop_are_bounded'),
    'callback': ('if module {', 'if module && false {', 'pending_callback_does_not_hold_batches'),
    'wire': ('delivery.wait().await;', '// skip reserved predecessor', 'wire_predecessor_cancel_and_drop_are_bounded'),
    'finish': ('if final_event {\n            self.forwarding.lock().unwrap().stopped = true;', 'if final_event && false {\n            self.forwarding.lock().unwrap().stopped = true;', 'finish_cancel_drop_stop_draining'),
    'dispose': ('self.forwarding.lock().unwrap().stopped = true;\n        self.inner.dispose()', 'self.inner.dispose()', 'stops_before_dispose_closes_port'),
    'drop': ('tokio::pin!(execution);', '''let inner = self.inner.clone();
        tokio::spawn(async move { loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let _ = inner.drain_events();
        }});
        tokio::pin!(execution);''', 'finish_cancel_drop_stop_draining'),
}
name = sys.argv[2]
old, new, test = mutations[name]
def run(label):
    slot = wait_for_slot(root / 'scripts/cargo-slots.sh')
    (out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
    result = run_owned(['cargo', 'test', '--offline', '-q', '-p', 'pilotdeck-gateway', '--lib', f'host_pump_{test}'], cwd=root / 'src', env=os.environ.copy(), timeout=900)
    (out / f'{label}.log').write_text(result.stdout + result.stderr)
    return result
with mutating_sources(root, [path]) as snapshots:
    content = snapshots[path].decode()
    assert content.count(old) == (2 if name == 'wire' else 1), (name, content.count(old))
    assert run('positive').returncode == 0
    path.write_text(content.replace(old, new))
    red = run('mutant')
    assert red.returncode != 0 and 'test result: FAILED' in red.stdout and 'panicked at' in red.stdout, 'must fail a test assertion, not compilation'
assert run('restored').returncode == 0
print(f'PASS {name}: positive / assertion failure / restored')

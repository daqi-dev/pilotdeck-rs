#!/usr/bin/env python3
"""Commit product changes first; select names after OUT for a bounded sample."""
import json
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR'):
    raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
live = root / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
fixtures = Path(__file__).resolve().parents[1]
controls = [
    ('unreserved-history', live, 'state.active = Some(cancellation.clone());', 'let _ = cancellation.clone();', 'live_history_slot_matrix', 'concurrent submit must reject before reading history'),
    ('empty-history', live, 'input.history = (self.history)();', 'input.history = (self.history)(); input.history.clear();', 'live_history_slot_matrix', 'new owner reads fresh history'),
    ('disposed-read', live, 'return Err(RuntimeError::new("Agent session disposed."));', 'let _ = (self.history)(); return Err(RuntimeError::new("Agent session disposed."));', 'live_history_slot_matrix', 'history slot matrix must match real TS'),
    ('rejected-read', live, 'if state.active.is_some() {', 'if state.active.is_some() { let _ = (self.history)();', 'live_history_slot_matrix', 'history slot matrix must match real TS'),
    ('gates-extra-row', fixtures/'turn-gates/oracle.json', '"matrix": [', '"matrix": [{"name":"unconsumed"},', 'gates_oracle_fully_consumed', 'oracle rows must be fully consumed'),
    ('gates-duplicate-row', fixtures/'turn-gates/oracle.json', '"matrix": [', '"matrix": [{"name":"start-failure"},', 'gates_oracle_fully_consumed', 'oracle rows must be fully consumed'),
    ('recording-extra-row', fixtures/'live-recording/oracle.json', '"matrix": [', '"matrix": [{"name":"unconsumed"},', 'live_recording_oracle_fully_consumed', 'oracle rows must be fully consumed'),
    ('recording-duplicate-row', fixtures/'live-recording/oracle.json', '"matrix": [', '"matrix": [{"name":"start-cancel"},', 'live_recording_oracle_fully_consumed', 'oracle rows must be fully consumed'),
]
selected = set(sys.argv[2:])
assert selected <= {c[0] for c in controls}
chosen = [c for c in controls if not selected or c[0] in selected]

def run(name, test):
    slots = wait_for_slot(root/'scripts/cargo-slots.sh', timeout=900)
    (out/f'{name}.slots.log').write_text(slots.stdout+slots.stderr)
    result = run_owned(['cargo','test','--offline','--manifest-path','src/Cargo.toml','-p','pilotdeck-gateway','--lib',test,'--','--test-threads=1'], cwd=root, env=env, timeout=900)
    (out/f'{name}.log').write_text(result.stdout+result.stderr)
    return result

summary = []
with mutating_sources(root, list(dict.fromkeys(c[1] for c in chosen))) as originals:
    for name, path, old, new, test, anchor in chosen:
        assert run(name+'-positive', test).returncode == 0
        source = originals[path].decode()
        assert source.count(old) == 1, (name, source.count(old))
        path.write_text(source.replace(old, new, 1))
        result = run(name, test)
        output = result.stdout+result.stderr
        assert result.returncode != 0 and 'test result: FAILED' in output, (name, output[-2000:])
        assert f'::{test} ... FAILED' in output and 'panicked at' in output and anchor in output, (name, output[-2000:])
        path.write_bytes(originals[path])
        assert run(name+'-restored', test).returncode == 0
        summary.append({'mutation':name, 'test':test, 'assertion':anchor})
        print(name, 'assertion RED; restored PASS', flush=True)
(out/'summary.json').write_text(json.dumps(summary, indent=2)+'\n')

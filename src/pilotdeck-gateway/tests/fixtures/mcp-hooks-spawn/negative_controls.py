#!/usr/bin/env python3
"""After coordinator commit: bounded #808 mutations, no private build directory."""
import argparse, os, re, sys
from pathlib import Path
root = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(root / 'src/pilotdeck-gateway/tests/fixtures'))
from negative_process import mutating_sources, run_owned, wait_for_slot
if not os.environ.get('CARGO_TARGET_DIR'):
    sys.exit('CARGO_TARGET_DIR must be supplied by the caller')
p = argparse.ArgumentParser()
p.add_argument('output')
p.add_argument('--only', action='append')
p.add_argument('--check-patterns', action='store_true')
a = p.parse_args()
out = Path(a.output).resolve()
out.mkdir(parents=True, exist_ok=False)
client = 'src/pilotdeck-gateway/src/runtime/mcp/client.rs'
hooks = 'src/pilotdeck-gateway/src/runtime/async_hooks.rs'
mutants = [
 ('start-clear', client, r'(impl ConnectCompletion.*?)(state.connect = None;)', r'\1let _ = &state;', 'start_spawn_failure_matrix', 'failure clears cached attempt'),
 ('start-drop', client, r'(impl Drop for ConnectCompletion.*?if )self.sender.is_some\(\)', r'\1false', 'start_spawn_drop_matrix', 'guard publishes an outcome'),
 ('start-identity', client, r'if state\s*\.connect\s*\.as_ref\(\)\s*\.is_some_and\(\|current\| current.same_channel\(&self.mine\)\)', 'if true', 'start_failure_keeps_replacement', 'old failure preserves replacement'),
 ('delivery-clear', hooks, r'\*self\s*\.pending\s*\.delivery\s*\.lock\(\)\s*\.unwrap_or_else\(std::sync::PoisonError::into_inner\) = None;', 'let _ = &self.pending;', 'delivery_spawn_failure_matrix', 'failure clears cached attempt'),
 ('delivery-drop', hooks, r'(impl Drop for DeliveryCompletion.*?if )self.sender.is_some\(\)', r'\1false', 'delivery_spawn_drop_matrix', 'guard publishes an outcome'),
 ('delivery-prepoll', hooks, r'(    let mut completion = DeliveryCompletion \{.*?\n    \};)\n    tokio::spawn\(async move \{', r'    tokio::spawn(async move {\n\1', 'delivery_spawn_drop_matrix', 'guard publishes an outcome'),
 ('delivery-poison', hooks, r'\*self\s*\.pending\s*\.delivery\s*\.lock\(\)\s*\.unwrap_or_else\(std::sync::PoisonError::into_inner\) = None;', 'if let Ok(mut slot) = self.pending.delivery.lock() { *slot = None; }', 'delivery_poison_failure', 'poisoned delivery cleared'),
 ('submit-clear', hooks, r'\*self\s*\.pending\s*\.delivery\s*\.lock\(\)\s*\.unwrap_or_else\(std::sync::PoisonError::into_inner\) = None;', 'let _ = &self.pending;', 'submit_sink_panic_clears_delivery', 'submit panic clears delivery'),
]
if a.only:
    assert set(a.only) <= {m[0] for m in mutants}, 'unknown mutant'
    mutants = [m for m in mutants if m[0] in a.only]
def save(name, value):
    with (out / name).open('x') as f: f.write(value)
def run(name, test):
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    save(name + '.slots.log', slots.stdout + slots.stderr)
    r = run_owned(['cargo', 'test', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway', '--lib', test], cwd=root, env=os.environ.copy(), timeout=600)
    log = r.stdout + r.stderr
    save(name + '.log', log)
    return r.returncode, log
for name, relative, pattern, replacement, test, anchor in mutants:
    path = root / relative
    if a.check_patterns:
        _, n = re.subn(pattern, replacement, path.read_text(), flags=re.S)
        assert n == 1, (name, n)
        print(f'{name}: unique source pattern; test={test}; anchor={anchor}', flush=True)
        continue
    with mutating_sources(root, [path]) as original:
        changed, n = re.subn(pattern, replacement, original[path].decode(), flags=re.S)
        assert n == 1, (name, n)
        path.write_text(changed)
        status, log = run(name, test)
        assert status == 101 and f'{test} ... FAILED' in log and 'test result: FAILED' in log and anchor in log, (name, 'not an anchored runtime failure')
    status, log = run(name + '-restored', test)
    assert status == 0 and f'{test} ... ok' in log, (name, 'restored test failed')
    print(name + ': killed and restored', flush=True)
